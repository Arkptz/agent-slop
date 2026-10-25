# agent-slop-protocol

Wire types of the `agent-slop-plugin/1` protocol: the JSON that `agent-slop`
exchanges with an external plugin process.

A plugin is any executable that answers two subcommands:

- `describe` prints a [`Manifest`] on stdout.
- `check` reads a [`Request`] on stdin and prints a [`Response`] on stdout.

The crate holds serde types only. It never spawns a process and has no
opinion on the language a plugin is written in. Unknown JSON fields are
ignored on read, so additive protocol changes stay compatible; a breaking
change bumps [`PROTOCOL_VERSION`].

See the plugin chapter of the agent-slop guide for the full contract.
