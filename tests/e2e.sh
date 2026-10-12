#!/bin/sh
# End-to-end tests: boots real microVMs and checks every control from inside.
#
#   tests/e2e.sh [path/to/zapper]
#
# Needs KVM, network access, a built `ubuntu-24.04` image (`make image`) and
# gcc for the attack probe. Runs its own daemon on a private config and socket
# under a temporary directory, so it does not touch a running daemon. Every
# session it starts is removed. Set ZAPPER_E2E_KEEP=1 to keep the directory.
set -u

Z=${1:-target/release/zapper}
Z=$(cd "$(dirname "$Z")" && pwd)/$(basename "$Z")
HERE=$(cd "$(dirname "$0")" && pwd)
T=$(mktemp -d /tmp/zapper-e2e.XXXXXX)
export ZAPPER_SOCKET=$T/api.sock ZAPPER_CONFIG=$T/zapper.yaml
fails=0

pass() { echo "ok    $1"; }
fail() { echo "FAIL  $1"; echo "      $2"; fails=$((fails + 1)); }
expect() { # name, haystack, needle
    case "$2" in *"$3"*) pass "$1" ;; *) fail "$1" "expected '$3' in: $(echo "$2" | head -c 300)" ;; esac
}

cleanup() {
    [ -n "${DAEMON:-}" ] && kill "$DAEMON" 2>/dev/null && wait "$DAEMON" 2>/dev/null
    [ -n "${ZAPPER_E2E_KEEP:-}" ] && echo "kept $T" || rm -rf "$T"
}
trap cleanup EXIT

mkdir -p "$T/work" "$T/attack"
echo 'SECRET=hunter2' > "$T/work/.env"
echo 'hello from host' > "$T/work/readme.txt"
gcc -static -O1 -o "$T/attack/attack" "$HERE/attack.c" 2>/dev/null || echo "skip  attack probe (no static gcc)"
cat > "$T/zapper.yaml" <<EOF
daemon: { state: $T/state, ui: "127.0.0.1:0", ask_timeout_secs: 20 }
secrets:
  test: { value: s3cr3t-test, hosts: [postman-echo.com] }
  key: { value: guest-visible-key, mode: guest }
policy:
  defaults: { net: deny, exec: allow }
profiles:
  default:
    engine: { image: ubuntu-24.04, vcpus: 1, memory_mib: 512, disk: 2GiB }
    agent: { kind: generic, command: [bash, -l], user: user, workdir: /work, env: { TOKEN: "{{secret:test}}", KEY: "{{secret:key}}" } }
    mounts: [{ host: "{workdir}", guest: /work, mode: rw }]
    policy:
      fs: [{ path: "/work/**/.env", access: hidden }]
      net:
        - { action: allow, host: example.com, ports: [443] }
        - { action: allow, host: postman-echo.com, ports: [443], methods: [GET], secrets: [test] }
        - { action: ask, host: "*.wikipedia.org" }
      exec: [{ action: deny, path: perl }]
  vsock:
    extends: default
    engine: { network: vsock }
  root:
    extends: default
    agent: { root: true }
EOF
chmod 600 "$T/zapper.yaml"

"$Z" serve > "$T/daemon.log" 2>&1 &
DAEMON=$!
for _ in 1 2 3 4 5 6 7 8 9 10; do [ -S "$ZAPPER_SOCKET" ] && break; sleep 0.3; done

echo "== isolation, policy and secrets (NIC)"
out=$("$Z" run -C "$T/work" -- bash -c '
code() { curl -sS -m 5 -o /dev/null -w "%{http_code}" "$@" 2>/dev/null; }
echo "id=$(id -u) ifaces=$(ls /sys/class/net | tr "\n" ,) proxy=${HTTPS_PROXY:-none}"
echo "readme=$(cat /work/readme.txt)"
echo "env=[$(cat /work/.env)]"
echo "token=$TOKEN key=$KEY"
echo "dns-allowed=$(getent hosts example.com | cut -d" " -f1)"
echo "dns-denied=[$(getent hosts github.com)]"
echo "tunnel=$(code https://example.com)"
echo "inject=$(curl -sS https://postman-echo.com/get -H "X-Token: $TOKEN" | grep -o "s3cr3t-test" | head -1)"
echo "post=$(curl -sS -X POST https://postman-echo.com/post)"
echo "port80=$(code http://example.com)"
echo "rawip=$(code https://1.1.1.1)"
echo "fronting=$(code --resolve github.com:443:$(getent hosts example.com | cut -d" " -f1) https://github.com)"
echo "perl=$(perl -e "print 1" 2>&1)"
exit 7' 2>&1)
code=$?
expect "workload runs as uid 1000 with a NIC, no proxy settings" "$out" "id=1000 ifaces=eth0,lo, proxy=none"
expect "workdir is copied in" "$out" "readme=hello from host"
expect "fs hidden rule" "$out" "env=[]"
expect "proxy secret is a placeholder, guest secret is the value" "$out" "token=zapper-secret-test key=guest-visible-key"
expect "DNS answers an allowed name with a synthetic address" "$out" "dns-allowed=198.18."
expect "DNS refuses a name no rule allows" "$out" "dns-denied=[]"
expect "allowed TLS connection" "$out" "tunnel=200"
expect "secret injected on an allowed request" "$out" "inject=s3cr3t-test"
expect "method rule: POST denied" "$out" "post=zapper: POST"
expect "port without a rule denied" "$out" "port80=000"
expect "raw IP denied" "$out" "rawip=000"
expect "TLS name must match the name dialled" "$out" "fronting=000"
expect "exec deny rule" "$out" "Permission denied"
[ "$code" = 7 ] && pass "exit code propagates" || fail "exit code propagates" "got $code"

out=$("$Z" run -C "$T/work" -- perl -e 1 2>&1)
code=$?
expect "a denied first command says so" "$out" "perl: exec denied by policy"
[ "$code" = 126 ] && pass "a denied first command exits 126" || fail "a denied first command exits 126" "got $code"

echo "== root in the guest cannot get around the NIC capture"
out=$("$Z" run -p root -C "$T/work" -- bash -c '
code() { curl -sS -m 5 -o /dev/null -w "%{http_code}" "$@" 2>/dev/null; }
echo "id=$(id -u)"
echo "nameserver 1.1.1.1" > /etc/resolv.conf
echo "own-dns=[$(getent hosts github.com)]"
echo "direct=$(code --resolve github.com:443:140.82.112.3 https://github.com)"
echo "gateway=$(code http://10.0.2.1:22)"' 2>&1)
expect "root shell" "$out" "id=0"
expect "DNS to another server is still answered by policy" "$out" "own-dns=[]"
expect "direct connection to a real address denied" "$out" "direct=000"
expect "nothing else on the gateway" "$out" "gateway=000"

echo "== vsock mode"
out=$("$Z" run -p vsock -C "$T/work" -- bash -c '
echo "ifaces=$(ls /sys/class/net | tr "\n" ,) proxy=$HTTPS_PROXY"
echo "tunnel=$(curl -sS -o /dev/null -w "%{http_code}" https://example.com)"
echo "direct=$(curl -sS --noproxy "*" -m 3 https://1.1.1.1 2>&1 | head -1)"' 2>&1)
expect "loopback only, proxy settings" "$out" "ifaces=lo, proxy=http://127.0.0.1:3128"
expect "allowed connection through the proxy" "$out" "tunnel=200"
expect "no direct egress" "$out" "Couldn't connect"

echo "== ask and approve"
"$Z" run --keep -C "$T/work" -- true > "$T/keep.log" 2>&1
id=$(grep -o 'session [a-z0-9]* kept' "$T/keep.log" | cut -d' ' -f2)
"$Z" exec "$id" -- curl -sS -o /dev/null -w '%{http_code}' https://en.wikipedia.org/ > "$T/ask.out" 2>&1 &
asker=$!
pending=""
for _ in 1 2 3 4 5 6 7 8 9 10; do
    pending=$("$Z" --json approvals | grep -o '"id": "[^"]*"' | head -1 | cut -d'"' -f4)
    [ -n "$pending" ] && break
    sleep 0.5
done
[ -n "$pending" ] && pass "request waits for approval" || fail "request waits for approval" "no pending approval"
"$Z" approve "$pending" --remember session > /dev/null
wait "$asker"
expect "approved request goes through" "$(cat "$T/ask.out")" "301"
expect "approval remembered as a session rule" "$("$Z" rules --scope "session:$id")" "en.wikipedia.org:443"
"$Z" rm "$id" > /dev/null

echo "== running and startup config"
"$Z" rule add global '{kind: net, action: deny, host: "*.evil.example", final: true}' > /dev/null
expect "rule change shows in diff" "$("$Z" config diff)" "*.evil.example"
expect "final global rule wins" "$("$Z" eval -p default net x.evil.example:443)" '"action": "deny"'
expect "invalid change is rejected" "$("$Z" rule add global '{kind: net, action: allow, secrets: [nope]}' 2>&1)" "unknown secret"
"$Z" config reload > /dev/null
expect "reload drops running changes" "$("$Z" config diff)" "no unsaved changes"

if [ -x "$T/attack/attack" ]; then
    echo "== attacks from inside the guest"
    out=$("$Z" run -C "$T/attack" -- /work/attack 2>&1)
    expect "cannot impersonate the agent on the control port" "$out" "host control port (impersonate agent)    refused"
    expect "cannot open a shell over vsock loopback" "$out" "own shell port via vsock loopback        refused"
    expect "cannot open a shell via own CID" "$out" "own shell port via own CID               refused"
    expect "32-bit execve is killed" "$out" "32-bit int 0x80 execve                   killed by signal 31"
    expect "x32 execve is killed" "$out" "x32 execve                               killed by signal 31"
    expect "denied exec fails" "$out" "native execve of denied perl             exited 42"
fi

echo
if [ "$fails" -eq 0 ]; then echo "all e2e checks passed"; else echo "$fails e2e checks failed; daemon log: $T/daemon.log"; ZAPPER_E2E_KEEP=1; fi
[ "$fails" -eq 0 ]
