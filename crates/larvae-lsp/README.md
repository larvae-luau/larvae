# larvae-lsp

The language server of [larvae](https://crates.io/crates/larvae): Luau's own
analysis frontend, vendored and compiled in, beside larvae's lints, formatter,
and code actions. One server, both halves.

```shell
cargo install larvae-lsp
```

The build compiles the vendored Luau C++ once, so it needs a C++ toolchain and
takes a few minutes. The binary carries its analyzer library inside and writes
it out on first run, so it works from wherever `cargo install` put it.

The [Larvae extension](https://marketplace.visualstudio.com/items?itemName=AndrewBordis.larvae)
starts it in VS Code; any other editor starts `larvae-lsp` over stdio, and
`larvae-lsp --new-solver` fixes Luau's new type solver on for a host with no
`larvae.toml`, and `--no-warning` publishes the errors alone. The
release archives of larvae ship the same binary, and `larvae self install`
puts it on PATH beside the CLI.

Everything it answers, and every `[lsp]` key of `larvae.toml`, is on the
[editor reference](https://larvae-luau.github.io/docs/reference/editor).
