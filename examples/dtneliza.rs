use anyhow::{bail, Result};
use bp7::flags::BlockControlFlags;
use bp7::{canonical, primary, Bundle, CreationTimestamp, EndpointID};
use clap::{crate_authors, crate_version, App, Arg};
use dtn7_plus::client::DtnClient;
use dtn7_plus::client::{Message, WsRecvData, WsSendData};
use dtn7_plus::sms::{SMSBundle, SmsBuilder};
use eliza::Eliza;
use std::convert::{TryFrom, TryInto};
use std::env;
use std::io::Write;
use std::time::Instant;

fn main() -> Result<()> {
    let matches = App::new("dtneliza")
        .version(crate_version!())
        .author(crate_authors!())
        .about("Eliza Bot for dtn-chat / SMS")
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
            Arg::with_name("verbose")
                .short("v")
                .long("verbose")
                .help("verbose output")
                .takes_value(false),
        )
        .arg(
            Arg::with_name("ipv6")
                .short("6")
                .long("ipv6")
                .help("Use IPv6")
                .takes_value(false),
        )
        .get_matches();

    let verbose: bool = matches.is_present("verbose");
    let port = std::env::var("DTN_WEB_PORT").unwrap_or_else(|_| "3000".into());
    let port = matches.value_of("port").unwrap_or(&port); // string is fine no need to parse number
    let localhost = if matches.is_present("ipv6") {
        "[::1]"
    } else {
        "127.0.0.1"
    };

    let client = DtnClient::with_host_and_port(
        localhost.into(),
        port.parse::<u16>().expect("invalid port number"),
    );
    let endpoint: String = if client
        .local_node_id()
        .expect("failed to get local node id")
        .scheme()
        == "dtn"
    {
        "sms".into()
    } else {
        "767".into()
    };
    client.register_application_endpoint(&endpoint)?;

    let mut wscon = client.ws()?;

    wscon.write_text("/bundle")?;
    let msg = wscon.read_text()?;
    if msg.starts_with("200 tx mode: bundle") {
        println!("[*] {}", msg);
    } else {
        bail!("[!] Failed to set mode to `bundle`");
    }

    wscon.write_text(&format!("/subscribe {}", endpoint))?;
    let msg = wscon.read_text()?;
    if msg.starts_with("200 subscribed") {
        println!("[*] {}", msg);
    } else {
        bail!("[!] Failed to subscribe to service");
    }

    let e_rules = include_str!("doctor.json");
    let mut eliza = Eliza::from_str(e_rules).unwrap();
    loop {
        let msg = wscon.read_message()?;
        match &msg {
            Message::Text(txt) => {
                if txt.starts_with("200") {
                    if verbose {
                        eprintln!("[<] {}", txt);
                    }
                } else {
                    println!("[!] {}", txt);
                }
            }
            Message::Binary(bin) => {
                let now = Instant::now();
                let raw_bundle: Bundle =
                    serde_cbor::from_slice(bin).expect("Error decoding Bundle from server");
                let sms_bundle =
                    SMSBundle::try_from(raw_bundle).expect("Error decoding SMS bundle from server");

                if verbose {
                    eprintln!(
                        "Bundle-Id: {} // From: {} / To: {}",
                        sms_bundle.id(),
                        sms_bundle.src().unwrap_or("dtn:none".into()),
                        sms_bundle.dst().unwrap()
                    );

                    let data_str = sms_bundle.msg();
                    eprintln!("Message: {}", data_str);
                } else {
                    print!(".");
                    std::io::stdout().flush().unwrap();
                }
                if sms_bundle.src().is_none() {
                    eprintln!("Cannot answer anonymous messages!");
                    continue;
                }
                // flip src and destionation
                let src_eid: EndpointID = sms_bundle.bundle().primary.destination.clone();
                let dst_eid: EndpointID = sms_bundle.bundle().primary.source.clone();

                let pblock = primary::PrimaryBlockBuilder::default()
                    .destination(dst_eid)
                    .source(src_eid)
                    .report_to(EndpointID::none())
                    .creation_timestamp(CreationTimestamp::now())
                    .lifetime(sms_bundle.bundle().primary.lifetime)
                    .build()
                    .unwrap();

                let payload = SmsBuilder::new()
                    .compression(sms_bundle.compression())
                    .message(&eliza.respond(sms_bundle.msg().as_str()))
                    .build()?;
                let cblocks = vec![canonical::new_payload_block(
                    BlockControlFlags::empty(),
                    serde_cbor::to_vec(&payload)
                        .expect("Fatal failure, could not convert sms payload to CBOR"),
                )];

                let mut response = SMSBundle::try_from(bp7::bundle::Bundle::new(pblock, cblocks))
                    .expect("error creating sms bundle"); // construct response with copied payload

                wscon
                    .write_binary(&response.to_cbor())
                    .expect("error sending echo response");
                if verbose {
                    println!("Processing bundle took {:?}", now.elapsed());
                }
            }
            _ => {
                if verbose {
                    eprintln!("[<] Other: {:?}", msg);
                }
            }
        }
    }
}
