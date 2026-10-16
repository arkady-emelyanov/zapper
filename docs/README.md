[← README](../README.md) · **Docs**

# Documentation

The Office:

- [Office](office.md): firms of AI workers, one VM each: mail and wake-ups, the shared drive, security officers, the cluster, the UI, replay.
- [Office prompts](office-prompts.md): the preamble, wake prompts, follow-ups, officers' prompts and an example kickoff.
- [Quickstart](quickstart.md): from a fresh clone to a running firm.

The machinery: zapper, the daemon, CLI and sandbox the office runs on:

- [Architecture](architecture.md): components, a session's life, channels, exec supervision, the office's parts in the daemon.
- [Network](network.md): NIC and vsock modes, DNS, how connections are decided, address checks.
- [Policy](policy.md): net, exec and fs rules, layering, secrets, approvals, dry runs.
- [Security model](security.md): threat model, where each control is enforced, the office's controls, limitations.
- [Configuration](configuration.md): the config file, profiles, agents, firms, and the running and startup config.
- [Images and kernels](images.md): building root filesystems from OCI images, and guest kernels.
- [Instrumentation](instrumentation.md): events, audit logs, terminal recordings and the UI.
- [API](api.md): the REST API on the unix socket, and the UI's API.
- [Commands](commands.md): `zapper` commands and `make` targets.
- [Testing](testing.md): unit and end-to-end tiers, the attack probe, manual scenarios.
