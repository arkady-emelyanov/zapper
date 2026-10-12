# zapper: policy-bound Firecracker sandboxes.
#
#   make deps              Firecracker and a guest kernel into ~/.local/share/zapper
#   make build             zapper and the static guest agent (release)
#   make install           both into ~/.local/bin
#   make image             the base image (IMAGE=ubuntu-24.04, see IMAGES below)
#   make image IMAGE=claude
#   make check             report missing prerequisites with the fix; changes nothing
#   make test              unit tests (no VMs)
#   make test-e2e          boots real VMs: policy, secrets, exec, attacks (needs KVM, network)
#   make clean
#
# Long targets print to the terminal; to follow them elsewhere:
#   make test-e2e > /tmp/zapper-e2e.log 2>&1 &  tail -f /tmp/zapper-e2e.log

MUSL    := x86_64-unknown-linux-musl
TARGET  := target/release
AGENT   := target/$(MUSL)/release/zapper-agent
PREFIX  ?= $(HOME)/.local
IMAGE   ?= ubuntu-24.04
ZAPPER  := $(TARGET)/zapper

# Image recipes: name → base and build steps.
BASE_PKGS := apt-get update && apt-get install -y --no-install-recommends ca-certificates curl git less procps && rm -rf /var/lib/apt/lists/*
IMAGE_FROM_ubuntu-24.04 := docker.io/library/ubuntu:24.04
IMAGE_RUN_ubuntu-24.04  := --run "$(BASE_PKGS)"
IMAGE_FROM_claude := docker.io/library/ubuntu:24.04
IMAGE_RUN_claude  := --run "$(BASE_PKGS) " --run "apt-get update && apt-get install -y --no-install-recommends ripgrep && rm -rf /var/lib/apt/lists/*" \
	--run "curl -fsSL https://claude.ai/install.sh | bash && cp -L /root/.local/bin/claude /usr/local/bin/claude && rm -rf /root/.local /root/.claude* && claude --version"
IMAGE_FROM_python := docker.io/library/python:3.13-slim
IMAGE_RUN_python  := --run "apt-get update && apt-get install -y --no-install-recommends ca-certificates curl git && rm -rf /var/lib/apt/lists/*"

.PHONY: deps build install image check test test-e2e clean

deps:
	scripts/fetch-deps.sh

build:
	cargo build --release -p zapper-agent --target $(MUSL)
	cargo build --release -p zapper
	cp $(AGENT) $(TARGET)/zapper-agent

install: build
	install -d $(PREFIX)/bin
	install -m 0755 $(TARGET)/zapper $(TARGET)/zapper-agent $(PREFIX)/bin/

image: build
	@test -n "$(IMAGE_FROM_$(IMAGE))" || { echo "no recipe for IMAGE=$(IMAGE); see IMAGE_FROM_* in the Makefile, or run zapper image build"; exit 1; }
	$(ZAPPER) image build $(IMAGE) --from $(IMAGE_FROM_$(IMAGE)) $(IMAGE_RUN_$(IMAGE))

check: build
	$(ZAPPER) check

test:
	cargo test --workspace

test-e2e: build
	tests/e2e.sh $(ZAPPER)

clean:
	cargo clean
