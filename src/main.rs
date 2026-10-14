mod ast;
mod builtins;
mod check;
mod cli;
mod diag;
mod error;
mod interp;
mod json;
mod lexer;
mod loader;
mod mounts;
mod parser;
mod pretty;
mod sandbox;
#[cfg(test)]
mod tests;
mod value;

fn main() {
    std::process::exit(cli::main_entry());
}
