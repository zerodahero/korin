# Install

korin is one binary, for macOS on Apple silicon and Linux on x86-64 or ARM64.

=== "mise"

    ```sh
    mise use github:zerodahero/korin
    ```

    This adds korin to the project's `mise.toml`, so everyone working on the project, and CI,
    gets the same version.

=== "From source"

    With a Rust toolchain:

    ```sh
    cargo install --locked --git https://github.com/zerodahero/korin
    ```

=== "Release archive"

    Download the archive for your platform from the
    [releases page](https://github.com/zerodahero/korin/releases), unpack it, and put `korin`
    on your `PATH`.

=== "Install script"

    ```sh
    curl --proto '=https' --tlsv1.2 -LsSf \
      https://github.com/zerodahero/korin/releases/latest/download/korin-installer.sh | sh
    ```

    The script downloads the binary for your platform into `~/.cargo/bin`. It is attached to
    each [release](https://github.com/zerodahero/korin/releases); read it first if you like.

Check that it works:

```sh
korin --version
```

## Editor support

A tree-sitter grammar gives Neovim and Helix highlighting for `.korin` files. See
[Editor support](../guides/editors.md).

Next: [Your first wizard](first-wizard.md).
