# Testnet systemd deployment

These templates persist the low-resource validator settings used for small
testnet instances. They are intended for 2 vCPU / 4 GiB hosts and keep ordinary
validators from running local STARK proof work.

Install on a host:

```bash
id -u shellchain >/dev/null 2>&1 || sudo useradd --system --home /var/lib/shell-chain --shell /usr/sbin/nologin shellchain
sudo install -d -o shellchain -g shellchain /mnt/shell-data /opt/shell
sudo install -m 0755 shell-node-start.sh /usr/local/bin/shell-node-start.sh
sudo install -m 0644 shell-node.service /etc/systemd/system/shell-node.service
sudo install -m 0644 shell-node.env.example /etc/default/shell-node
sudo systemctl daemon-reload
sudo systemctl enable --now shell-node
```

Operational defaults:

- `SHELL_NODE_ROLE=validator`
- `SHELL_ENABLE_STARK_AGGREGATION=false`
- `SHELL_STATE_CACHE_SIZE_MB=32`
- `SHELL_RPC_RATE_LIMIT=50`
- `SHELL_RPC_ADDR=127.0.0.1:8545`
- `SHELL_MAX_IDLE_INTERVAL_SECS=600`
- systemd `MemoryMax=1900M`, `CPUQuota=90%`, low IO priority, and slow restart

Use a separate larger instance for proving. On that host set
`SHELL_NODE_ROLE=validator-prover` or `SHELL_NODE_ROLE=prover`, set
`SHELL_ENABLE_STARK_AGGREGATION=true`, and raise the systemd memory/CPU limits
to match the instance size.
