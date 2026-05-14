# markview

`markview` is a small Markdown viewer written in idiomatic Rust. It keeps Markdown parsing and frontend rendering separate, so the same core can target a terminal, a native macOS WebKit window, or another renderer later.

## Install locally

```sh
cargo install --path .
```

To build the macOS GUI binary:

```sh
cargo build --features gui --bin markview-gui
```

## Usage

```sh
markview README.md
cat README.md | markview --no-color
markview --width 72 notes.md

cargo run --features gui --bin markview-gui -- README.md
```

## Design

- Pluggable frontends: `FrontendRenderer` separates document input from terminal and HTML output.
- Proper GUI rendering: the GUI path renders Markdown to HTML/CSS and displays it in the system WebKit WebView on macOS.
- Portable core: parsing and rendering live in the library; GUI dependencies are optional behind the `gui` feature.
- Fast enough for local viewing: the terminal renderer is single-pass, and the GUI uses the OS web engine instead of embedding a browser runtime.
- Tested: terminal rendering, HTML rendering, CLI behavior, and frontend substitution are covered by unit and integration tests.
