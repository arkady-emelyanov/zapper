/*
 * attack.c: probes zapper's in-guest controls from inside a session, as the
 * workload would.
 *
 * tests/e2e.sh compiles it statically (gcc -static, so it needs nothing from
 * the guest image), copies it into a session's /work and runs it as the
 * workload: uid 1000, under the agent's seccomp filter, with an exec rule that
 * denies perl. Each probe prints one line; e2e.sh matches those lines, so keep
 * their wording when changing this file.
 *
 * What it tries, and what must happen:
 *
 *   vsock to the host's control port (CID 2, port 1000)
 *       Pose as the guest agent and talk to the daemon's control channel.
 *       The daemon accepts only the first connection on that port, made by
 *       the agent at boot, then removes the socket: refused.
 *
 *   vsock to the guest's own shell port (CID 1 loopback, or its own CID 3)
 *       Open a terminal as root without exec supervision ("system": true) by
 *       talking to the agent directly. The agent accepts terminal connections
 *       only from the host's CID: refused.
 *
 *   vsock to the host's port 1024
 *       Nothing listens there for guests: refused.
 *
 *   execve through the 32-bit (int 0x80) and x32 syscall tables
 *       Bypass the exec check, which the filter attaches to the native
 *       execve and execveat. The filter kills any foreign-arch or x32
 *       syscall: killed by SIGSYS (31).
 *
 *   native execve of perl
 *       The exec check asks the host, whose rule denies perl: execve fails,
 *       and the child exits 42.
 *
 * A line saying CONNECTED or EXECUTED! means a control failed.
 *
 *   gcc -static -O1 -o attack tests/attack.c
 */

#include <stdio.h>
#include <string.h>
#include <errno.h>
#include <unistd.h>
#include <sys/socket.h>
#include <sys/wait.h>
#include <linux/vm_sockets.h>
#include <sys/syscall.h>

/* Connects to cid:port over vsock and, if that works, sends a terminal
 * request as the host would, asking for a root shell without supervision. */
static void try_vsock(unsigned cid, unsigned port, const char *what) {
    int s = socket(AF_VSOCK, SOCK_STREAM, 0);
    struct sockaddr_vm a = { .svm_family = AF_VSOCK, .svm_cid = cid, .svm_port = port };
    int r = connect(s, (struct sockaddr *)&a, sizeof a);
    if (r == 0) {
        const char *req = "{\"id\":1,\"argv\":[\"/bin/sh\",\"-c\",\"id\"],\"tty\":null,\"system\":true}\n";
        write(s, req, strlen(req));
        char buf[256] = {0};
        int n = read(s, buf, sizeof buf - 1);
        printf("%-40s CONNECTED, read %d bytes\n", what, n);
    } else {
        printf("%-40s refused: %s\n", what, strerror(errno));
    }
    close(s);
}

/* Runs f in a child: an exec that works replaces the child (and /bin/true
 * exits 0, "EXECUTED!"); one that fails falls through to _exit(42); a killed
 * child reports its signal. */
static void try_exec(const char *what, void (*f)(void)) {
    pid_t p = fork();
    if (p == 0) { f(); _exit(42); }
    int st; waitpid(p, &st, 0);
    if (WIFSIGNALED(st)) printf("%-40s killed by signal %d\n", what, WTERMSIG(st));
    else printf("%-40s exited %d%s\n", what, WEXITSTATUS(st), WEXITSTATUS(st) == 42 ? " (exec failed)" : " (EXECUTED!)");
}

static char *argv_[] = { "/bin/true", 0 };
static void int80(void) {
    // 32-bit execve (nr 11) through the compat entry point.
    long r; asm volatile("int $0x80" : "=a"(r) : "a"(11), "b"("/bin/true"), "c"(argv_), "d"(0) : "memory");
}
/* x32 execve: the x32 ABI's syscall number 520 with the x32 bit set. */
static void x32(void) { syscall(0x40000000 | 520, "/bin/true", argv_, 0); }
/* An ordinary execve of a binary the e2e config's exec rule denies. */
static void native(void) { execve("/usr/bin/perl", (char *[]){"perl", "-e", "1", 0}, 0); }

int main(void) {
    printf("uid %d\n", getuid());
    try_vsock(2, 1000, "host control port (impersonate agent)");
    try_vsock(1, 1024, "own shell port via vsock loopback");
    try_vsock(3, 1024, "own shell port via own CID");
    try_vsock(2, 1024, "host port 1024");
    try_exec("32-bit int 0x80 execve", int80);
    try_exec("x32 execve", x32);
    try_exec("native execve of denied perl", native);
    return 0;
}
