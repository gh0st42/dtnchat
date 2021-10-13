extern crate linefeed;

use anyhow::Result;
use bp7::EndpointID;
use clap::{crate_authors, crate_version, App, Arg};
use crossbeam_channel::{unbounded, Receiver, Sender};
use dtn7_plus::client::{DtnClient, WsSendData};
use dtn7_plus::sms::SmsBuilder;
use humantime::{format_duration, parse_duration};
use linefeed::chars::escape_sequence;
use linefeed::command::COMMANDS;
use linefeed::complete::{Completer, Completion};
use linefeed::inputrc::parse_text;
use linefeed::terminal::Terminal;
use linefeed::{Interface, Prompter, ReadResult};
use std::convert::{TryFrom, TryInto};
use std::io;
use std::io::{stdout, Read, Write};
use std::sync::Arc;
use std::thread;
use std::{collections::HashSet, time::Duration};
use termion::color::{AnsiValue, Bg, DetectColors};
use termion::raw::IntoRawMode;
use termion::{clear, color::*, style};
use ws::Builder;

use dtnchat::ws::*;

const HISTORY_FILE: &str = "linefeed.hst";

fn print_logo() {
    println!("{}", clear::All);
    /*let count;
    {
        let mut term = stdout().into_raw_mode().unwrap();
        count = term.available_colors().unwrap();
    }*/

    //println!("This terminal supports {} colors.", count);
    //for i in 0..count {
    //print!("{} {}", Bg(AnsiValue(i as u8)), Bg(AnsiValue(0)));
    //}
    //println!();

    let logo = include_str!("../logo.txt");
    let mut line_color = 0;
    for line in logo.split("\n") {
        println!("  {}{}", Fg(AnsiValue::rgb(0, line_color, 0)), line);
        line_color = (line_color + 1) % 6;
    }
    println!("{}", style::Reset);
    //println!("{}{}{}", Fg(LightBlue), logo, style::Reset);

    println!("This is a simple dtn chat and messaging program.");
    println!("Enter \"/help\" for a list of commands.");
    println!("Press Ctrl-D or enter \"/quit\" to exit.");
    println!("");
}
fn pe(c: String) -> String {
    format!("\x01{}\x02", c)
}
fn send_sms(
    tx: Sender<WsCommand>,
    src: EndpointID,
    dst: EndpointID,
    lifetime: Duration,
    msg: &str,
) -> Result<()> {
    let sms = serde_cbor::to_vec(
        &SmsBuilder::new()
            .compression(true)
            .message(msg.trim())
            .build()?,
    )?;
    let data = WsSendData {
        src,
        dst,
        delivery_notification: true,
        lifetime,
        data: sms,
    };
    tx.send(WsCommand::SendData(data))?;
    Ok(())
}
fn main() -> Result<()> {
    print_logo();
    let matches = App::new("dtnchat")
        .version(crate_version!())
        .author(crate_authors!())
        .about("A simple Bundle Protocol 7 Delay Tolerant Networking SMS Chat")
        .arg(
            Arg::with_name("port")
                .short("p")
                .long("port")
                .value_name("PORT")
                .help("Local web port (default = 3000)")
                .required(false)
                .takes_value(true),
        )
        .arg(
            Arg::with_name("ipv6")
                .short("6")
                .long("ipv6")
                .help("Use IPv6")
                .takes_value(false),
        )
        .arg(
            Arg::with_name("verbose")
                .short("v")
                .long("verbose")
                .help("Verbose output")
                .takes_value(false),
        )
        .get_matches();

    let port = std::env::var("DTN_WEB_PORT").unwrap_or_else(|_| "3000".into());
    let port = matches.value_of("port").unwrap_or(&port); // string is fine no need to parse number

    let localhost = if matches.is_present("ipv6") {
        "[::1]"
    } else {
        "127.0.0.1"
    };
    let verbose = matches.is_present("verbose");

    let client = DtnClient::with_host_and_port(
        localhost.into(),
        port.parse::<u16>().expect("invalid port number"),
    );
    let interface = Arc::new(Interface::new("dtnchat")?);

    let mut query: Option<EndpointID> = None;
    let localnode: EndpointID = client.local_node_id()?;
    let endpoint = localnode.new_endpoint("sms")?;
    let endpoint2 = endpoint.clone();
    client.register_application_endpoint(&endpoint.to_string())?;
    let iface = interface.clone();
    let (tx, rx) = unbounded::<WsCommand>();
    //let thread_rx = rx.clone();
    //let mut ws = new_chat_connection(rx.clone(), iface.clone(), verbose, endpoint.clone());
    let mut ws = Builder::new()
        .build(move |out: ws::Sender| {
            let out2 = out.clone();
            let rx2 = rx.clone();
            let iface2 = iface.clone();
            thread::spawn(move || {
                send_listener(rx2.clone(), out2.clone(), iface2.clone(), verbose)
            });

            ChatConnection {
                localnode: endpoint2.clone(),
                out,
                subscribed: false,
                verbose,
                iface: iface.clone(),
                recv: rx.clone(),
            }
        })
        .unwrap();
    ws.connect(url::Url::parse(&format!("ws://{}:{}/ws", localhost, port))?)?;

    let mut peers: HashSet<String> = HashSet::new();
    //eids.insert("node3".into());

    let completer = Arc::new(DtnChatCompleter {
        eids: peers.clone(),
    });
    interface.set_completer(completer);
    interface.set_prompt(&format!(
        "{}{} {}> {}",
        pe(Fg(LightBlue).to_string()),
        localnode.node().unwrap(),
        pe(Fg(LightWhite).to_string()),
        pe(termion::style::Reset.to_string())
    ))?;

    if let Err(e) = interface.load_history(HISTORY_FILE) {
        if e.kind() == io::ErrorKind::NotFound {
            println!(
                "History file {} doesn't exist, not loading history.",
                HISTORY_FILE
            );
        } else {
            eprintln!("Could not load history file {}: {}", HISTORY_FILE, e);
        }
    }

    println!("");
    let _handler = thread::spawn(|| {
        ws.run().unwrap();
    });

    let mut groups: HashSet<String> = HashSet::new();
    let mut lifetime: Duration = Duration::from_secs(60 * 60);

    while let ReadResult::Input(line) = interface.read_line()? {
        if !line.trim().is_empty() {
            interface.add_history_unique(line.clone());
        }

        let (cmd, args) = split_first_word(&line);
        match cmd {
            "/help" => {
                println!("dtnchat commands:");
                println!();
                for &(cmd, help) in DTNCHAT_COMMANDS {
                    println!("  {:15} - {}", cmd, help);
                }
                println!();
            }
            "/lifetime" => {
                println!("Current bundle lifetime: {}", format_duration(lifetime));
                if args.len() > 1 {
                    if let Ok(new_lifetime) = parse_duration(args) {
                        lifetime = new_lifetime;
                        println!("New bundle lifetime: {}", format_duration(lifetime));
                    } else {
                        println!("Invalid lifetime duration format!");
                    }
                }
            }
            "/join" => {
                let dst: EndpointID = if let Ok(dst_num) = args.parse::<u64>() {
                    format!("ipn://{}.767", dst_num).try_into()?
                } else {
                    format!("dtn://{}/sms", args).try_into()?
                };
                client.register_application_endpoint(&dst.to_string())?;
                if peers.insert(dst.node().unwrap()) {
                    let completer = Arc::new(DtnChatCompleter {
                        eids: peers.clone(),
                    });
                    interface.set_completer(completer);
                }
                groups.insert(dst.node().unwrap());
                tx.send(WsCommand::Text(format!("/subscribe {}", dst)))?;
            }
            "/leave" => {
                if args != localnode.node().unwrap() {
                    let dst: EndpointID = if let Ok(dst_num) = args.parse::<u64>() {
                        format!("ipn://{}.767", dst_num).try_into()?
                    } else {
                        format!("dtn://{}/sms", args).try_into()?
                    };
                    client.unregister_application_endpoint(&dst.to_string())?;
                    if peers.remove(&dst.node().unwrap()) {
                        let completer = Arc::new(DtnChatCompleter {
                            eids: peers.clone(),
                        });
                        interface.set_completer(completer);
                    }
                    groups.remove(&dst.node().unwrap());
                    tx.send(WsCommand::Text(format!("/unsubscribe {}", dst)))?;
                }
            }
            "/list" => {
                println!("currently joined groups:");
                for i in &groups {
                    println!("  {}", i);
                }
                println!("");
            }
            "/query" => {
                if args == "" {
                    query = None;
                    interface.set_prompt(&format!(
                        "{}{} {}> {}",
                        pe(Fg(LightBlue).to_string()),
                        localnode.node().unwrap(),
                        pe(Fg(LightWhite).to_string()),
                        pe(termion::style::Reset.to_string())
                    ))?;
                } else {
                    let (dst_node, msg) = split_first_word(&args);
                    let dst: EndpointID = if let Ok(dst_num) = dst_node.parse::<u64>() {
                        format!("ipn://{}.767", dst_num).try_into()?
                    } else {
                        format!("dtn://{}/sms", dst_node).try_into()?
                    };
                    if !peers.contains(&dst.node().unwrap()) {
                        peers.insert(dst.node().unwrap());
                        let completer = Arc::new(DtnChatCompleter {
                            eids: peers.clone(),
                        });
                        interface.set_completer(completer);
                    }
                    query = Some(dst);
                    interface.set_prompt(&format!(
                        "{}{} {}>> {}{} {}> {}",
                        pe(Fg(LightBlue).to_string()),
                        localnode.node().unwrap(),
                        pe(Fg(LightWhite).to_string()),
                        pe(Fg(LightCyan).to_string()),
                        query.clone().unwrap().node().unwrap(),
                        pe(Fg(LightWhite).to_string()),
                        pe(termion::style::Reset.to_string())
                    ))?;
                }
            }
            "/msg" => {
                //println!("msg: {}", args);
                let (dst_node, msg) = split_first_word(&args);
                let dst: EndpointID = if let Ok(dst_num) = dst_node.parse::<u64>() {
                    format!("ipn://{}.767", dst_num).try_into()?
                } else {
                    format!("dtn://{}/sms", dst_node).try_into()?
                };
                if !peers.contains(&dst.node().unwrap()) {
                    peers.insert(dst.node().unwrap());
                    let completer = Arc::new(DtnChatCompleter {
                        eids: peers.clone(),
                    });
                    interface.set_completer(completer);
                }
                send_sms(tx.clone(), endpoint.clone(), dst, lifetime, msg)?;
            }
            "/peers" => {
                println!("known peers: {}", args);
                for i in peers.iter() {
                    println!("  {}", i);
                }
            }
            "/history" => {
                let w = interface.lock_writer_erase()?;

                for (i, entry) in w.history().enumerate() {
                    println!("{}: {}", i, entry);
                }
            }
            "/save-history" => {
                if let Err(e) = interface.save_history(HISTORY_FILE) {
                    eprintln!("Could not save history file {}: {}", HISTORY_FILE, e);
                } else {
                    println!("History saved to {}", HISTORY_FILE);
                }
            }
            "/quit" => break,
            _ => {
                if line.starts_with("/") {
                    println!("Unknown command: {:?}", line);
                } else if query.is_none() {
                    println!("Please open query first");
                } else {
                    send_sms(
                        tx.clone(),
                        endpoint.clone(),
                        query.clone().unwrap(),
                        lifetime,
                        &line,
                    )?;
                }
            }
        }
    }

    println!("Goodbye.");

    Ok(())
}

fn split_first_word(s: &str) -> (&str, &str) {
    let s = s.trim();

    match s.find(|ch: char| ch.is_whitespace()) {
        Some(pos) => (&s[..pos], s[pos..].trim_start()),
        None => (s, ""),
    }
}

static DTNCHAT_COMMANDS: &[(&str, &str)] = &[
    ("/query", "Open query to specific endpoint"),
    ("/msg", "Compose a new short message"),
    ("/list", "List subscriptions"),
    ("/lifetime", "Manage message lifetime"),
    ("/peers", "List known peers"),
    ("/join", "Join a group"),
    ("/leave", "Leave a group"),
    ("/help", "You're looking at it"),
    ("/history", "Print history"),
    ("/save-history", "Write history to file"),
    ("/quit", "Quit"),
];

struct DtnChatCompleter {
    eids: HashSet<String>,
}

impl<Term: Terminal> Completer<Term> for DtnChatCompleter {
    fn complete(
        &self,
        word: &str,
        prompter: &Prompter<Term>,
        start: usize,
        _end: usize,
    ) -> Option<Vec<Completion>> {
        let line = prompter.buffer();

        let mut words = line[..start].split_whitespace();

        match words.next() {
            // Complete command name
            None => {
                let mut compls = Vec::new();

                for &(cmd, _) in DTNCHAT_COMMANDS {
                    if cmd.starts_with(word) {
                        compls.push(Completion::simple(cmd.to_owned()));
                    }
                }

                Some(compls)
            }
            // Complete command parameters
            Some("/get") | Some("/set") => {
                if words.count() == 0 {
                    let mut res = Vec::new();

                    for (name, _) in prompter.variables() {
                        if name.starts_with(word) {
                            res.push(Completion::simple(name.to_owned()));
                        }
                    }

                    Some(res)
                } else {
                    None
                }
            }
            // Complete command parameters
            Some("/query") | Some("/msg") => {
                if words.count() == 0 {
                    let mut res = Vec::new();

                    for name in self.eids.iter() {
                        if name.starts_with(word) {
                            res.push(Completion::simple(name.to_owned()));
                        }
                    }

                    Some(res)
                } else {
                    None
                }
            }
            _ => None,
        }
    }
}
