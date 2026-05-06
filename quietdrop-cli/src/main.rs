mod repl;

use quietdrop_core::client;
use quietdrop_core::encryption::generate_keypair;
use quietdrop_core::message::{Message, MessageType};
use quietdrop_core::server;
use sodiumoxide::crypto::box_;
use std::env;
use std::fs::File;
use std::io::prelude::*;
use tokio::runtime::Runtime;

use crate::repl::{ReadOutcome, Repl};

fn main() {
    quietdrop_core::initialize();

    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: quietdrop <server|client> [--no-history]");
        std::process::exit(1);
    }

    let rt = Runtime::new().expect("Failed to create Tokio runtime");
    let no_history = args.iter().any(|a| a == "--no-history");

    match args[1].as_str() {
        "server" => run_server(&rt),
        "client" => run_client(&rt, !no_history),
        _ => {
            eprintln!("Invalid argument. Use 'client' or 'server'.");
            std::process::exit(1);
        }
    }
}

fn run_server(rt: &Runtime) {
    let (public_key, secret_key) = generate_keypair();

    let server_public_key_bytes = public_key.as_ref().to_vec();
    let server_secret_key_bytes = secret_key.as_ref().to_vec();

    let _ = File::create("server_public_key.key")
        .and_then(|mut file| file.write_all(&server_public_key_bytes));
    let _ = File::create("server_secret_key.key")
        .and_then(|mut file| file.write_all(&server_secret_key_bytes));

    let server_secret_key = box_::SecretKey::from_slice(&server_secret_key_bytes).unwrap();

    println!("\n>>> Now listening for incoming messages...\n");

    rt.block_on(server::run_server("127.0.0.1:8080", &server_secret_key))
        .expect("Server failed to run");
}

fn run_client(rt: &Runtime, persist_history: bool) {
    // Generate the client keypair
    let (public_key, secret_key) = generate_keypair();

    // Load server's public key from the file written by the server side
    let mut file = File::open("server_public_key.key")
        .expect("Unable to open the key file (run the server first)");

    let mut server_public_key_bytes = Vec::new();
    file.read_to_end(&mut server_public_key_bytes)
        .expect("Unable to read the key file");

    let server_public_key = box_::PublicKey::from_slice(&server_public_key_bytes).unwrap();

    let mut repl = Repl::new(persist_history).expect("Failed to initialize line editor");
    print_banner(&repl);

    // Ask for the user's name once. Names are user-identifying but not secret
    // and we deliberately don't persist them.
    let name = match repl.readline("Enter your name: ") {
        ReadOutcome::Line(s) if !s.is_empty() => s,
        _ => {
            println!("No name provided, exiting.");
            return;
        }
    };

    // Default recipient; can be changed at runtime with /recipient.
    let mut recipient = String::from("Bob");

    println!(
        "\nType a message and press Enter to send it.\n\
         Type /help for commands. Up/Down/Ctrl+R recall previous *commands*\n\
         (message bodies are never stored).\n"
    );

    loop {
        let prompt = format!("{name}@{recipient}> ");

        // Use a single readline call; route based on the first character.
        // Free-form messages use the non-persisting variant; commands use
        // the variant that records to history.
        let outcome = repl.readline(&prompt);
        match outcome {
            ReadOutcome::Line(line) => {
                if line.is_empty() {
                    continue;
                }
                if line.starts_with('/') {
                    // Re-record this exact command in history (the prior
                    // readline didn't, since we used the message variant).
                    repl.record_command(&line);
                    match handle_command(&line, &mut repl, &mut recipient) {
                        CommandResult::Continue => continue,
                        CommandResult::Quit => break,
                    }
                } else {
                    send_text(
                        rt,
                        &name,
                        &recipient,
                        &line,
                        &server_public_key,
                        &secret_key,
                        public_key,
                    );
                }
            }
            ReadOutcome::Interrupted => {
                println!("(Ctrl+C — type /quit or press Ctrl+D to exit)");
            }
            ReadOutcome::Eof => {
                println!("Goodbye.");
                break;
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn send_text(
    rt: &Runtime,
    sender: &str,
    recipient: &str,
    body: &str,
    server_public_key: &box_::PublicKey,
    sender_secret_key: &box_::SecretKey,
    sender_public_key: box_::PublicKey,
) {
    let mut msg = Message {
        timestamp: chrono::Utc::now(),
        message_type: MessageType::Text,
        sender: sender.to_owned(),
        recipient: recipient.to_owned(),
        content: vec![],
        public_key: sender_public_key,
    };
    msg.encrypt_content(body, server_public_key, sender_secret_key);

    if let Err(e) = rt.block_on(client::send_message(&msg, "127.0.0.1:8080")) {
        eprintln!("Failed to send message: {e}");
    }
}

enum CommandResult {
    Continue,
    Quit,
}

fn handle_command(line: &str, repl: &mut Repl, recipient: &mut String) -> CommandResult {
    let mut parts = line.splitn(2, char::is_whitespace);
    let cmd = parts.next().unwrap_or("");
    let arg = parts.next().unwrap_or("").trim();

    match cmd {
        "/help" | "/?" => print_help(),
        "/quit" | "/exit" => return CommandResult::Quit,
        "/clear" => {
            print!("\x1B[2J\x1B[H");
            let _ = std::io::Write::flush(&mut std::io::stdout());
        }
        "/history" => {
            let snapshot = repl.history_snapshot();
            if snapshot.is_empty() {
                println!("(no command history yet)");
            } else {
                for (i, entry) in snapshot.iter().enumerate() {
                    println!("{:>4}  {}", i + 1, entry);
                }
            }
        }
        "/clear-history" => {
            repl.clear_history();
            println!("History cleared (file: {}).", repl.history_path().display());
        }
        "/recipient" | "/to" => {
            if arg.is_empty() {
                println!("Current recipient: {recipient}");
                println!("Usage: /recipient <name>");
            } else {
                *recipient = arg.to_owned();
                println!("Recipient set to {recipient}.");
            }
        }
        other => {
            println!("Unknown command: {other}. Type /help for a list of commands.");
        }
    }
    CommandResult::Continue
}

fn print_banner(repl: &Repl) {
    println!("QuietDrop CLI — interactive client");
    if repl.is_persistent() {
        println!("Command history: {}", repl.history_path().display());
    } else {
        println!("Command history: disabled (--no-history)");
    }
}

fn print_help() {
    println!(
        "Available commands:\n  \
         /help, /?              Show this help\n  \
         /quit, /exit           Exit the client (Ctrl+D also works)\n  \
         /clear                 Clear the screen\n  \
         /history               Show command history (Up/Down also works)\n  \
         /clear-history         Wipe history (memory + file)\n  \
         /recipient <name>      Change the message recipient\n  \
         /to <name>             Alias for /recipient\n\n\
         Anything not starting with '/' is sent as an encrypted message.\n\
         Message bodies are never written to disk; only commands are.\n\
         Pass --no-history on launch for a fully ephemeral session."
    );
}
