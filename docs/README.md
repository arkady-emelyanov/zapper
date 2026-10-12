[← README](../README.md) · **Docs**

# Documentation

Using zapper:

- [Commands](commands.md): `zapper` commands and `make` targets.
- [Configuration](configuration.md): the config file, profiles, agents, and the running and startup config.
- [Network](network.md): NIC and vsock modes, DNS, how connections are decided, address checks.
- [Policy](policy.md): net, exec and fs rules, layering, secrets, approvals, dry runs.
- [Images and kernels](images.md): building root filesystems from OCI images, and guest kernels.
- [Instrumentation](instrumentation.md): events, audit logs, terminal recordings and the UI.
- [API](api.md): the REST API on the unix socket, and the read-only UI API.

How it works:

- [Architecture](architecture.md): components, a session's life, channels, exec supervision.
- [Security model](security.md): threat model, where each control is enforced, limitations.
- [Testing](testing.md): unit and end-to-end tiers, the attack probe, manual scenarios.
