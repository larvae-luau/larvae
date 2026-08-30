#!/usr/bin/env bash
cargo build -p larvae -p larvae-lsp --features larvae-lsp/analyzer 
target/debug/larvae self install