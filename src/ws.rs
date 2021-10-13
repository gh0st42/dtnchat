use anyhow::Result;
use bp7::{dtntime::DtnTimeHelpers, Bundle, EndpointID};
use chrono::{Local, TimeZone};
use crossbeam_channel::Receiver;
use dtn7_plus::client::WsSendData;
use dtn7_plus::sms::SMSBundle;
use linefeed::terminal::DefaultTerminal;
use linefeed::Interface;
use std::convert::{TryFrom, TryInto};
use std::error::Error;
use std::io;
use std::io::{stdout, Read, Write};
use std::sync::Arc;
use std::thread;
use std::{collections::HashSet, time::Duration};
use termion::{color::*, style};
use ws::{Builder, CloseCode, Handler, Handshake, Message, Sender};

pub struct ChatConnection {
    pub localnode: EndpointID,
    pub out: Sender,
    pub subscribed: bool,
    pub verbose: bool,
    pub iface: Arc<Interface<DefaultTerminal>>,
    pub recv: Receiver<WsCommand>,
}

pub struct Outgoing {
    pub src: EndpointID,
    pub dst: EndpointID,
    pub delivery_notification: bool,
    pub lifetime: Duration,
    pub data: Vec<u8>,
}
pub enum WsCommand {
    Text(String),
    SendData(Outgoing),
}
pub fn send_listener(
    recv: Receiver<WsCommand>,
    out: Sender,
    iface: Arc<Interface<DefaultTerminal>>,
    verbose: bool,
) {
    for data in &recv {
        match data {
            WsCommand::Text(cmd) => {
                out.send(cmd).expect("error sending command");
            }
            WsCommand::SendData(data) => {
                let flags = if data.delivery_notification && !data.dst.to_string().contains("sms2")
                {
                    //println!("Delivery notification requested");
                    bp7::flags::BundleControlFlags::BUNDLE_STATUS_REQUEST_DELIVERY.bits()
                } else {
                    0
                };
                let mut bndl = bp7::bundle::BundleBuilder::new()
                    .primary(
                        bp7::primary::PrimaryBlockBuilder::new()
                            .source(data.src)
                            .destination(data.dst)
                            .lifetime(data.lifetime)
                            .bundle_control_flags(flags)
                            .creation_timestamp(bp7::CreationTimestamp::now())
                            .build()
                            .unwrap(),
                    )
                    .payload(data.data)
                    .build()
                    .unwrap();
                //println!("{:?}", bndl);
                let out_bytes = bndl.to_cbor();
                if verbose {
                    writeln!(
                        iface,
                        "{}Sent bundle with {} bytes.{}",
                        Fg(Yellow),
                        out_bytes.len(),
                        style::Reset
                    )
                    .unwrap();
                }
                out.send(out_bytes).expect("error sending echo response");
            }
        }
    }
}

impl ChatConnection {
    fn on_bundle(&self, bndl: Bundle) -> Result<()> {
        if bndl.is_administrative_record() {
            if self.verbose {
                writeln!(
                    self.iface,
                    "{}Handling of administrative records not yet implemented!{}",
                    Fg(Red),
                    style::Reset,
                )?;
            }
        } else if self.verbose || bndl.primary.source != self.localnode {
            if let Ok(smsbundle) = SMSBundle::try_from(bndl) {
                /*if self.verbose {
                    writeln!(self.iface, "Bundle-Id: {}", bndl.id());
                }*/
                //let message = std::str::from_utf8(&data).unwrap().trim();
                let message = smsbundle.msg();
                let unixtime = smsbundle.creation_timestamp().dtntime().unix();
                //let rfc3339 = bndl.primary.creation_timestamp.dtntime().string();
                //let seq_no = bndl.primary.creation_timestamp.seqno();
                let datetime = Local.timestamp(unixtime as i64, 0);
                if smsbundle.dst().unwrap() == self.localnode.node().unwrap() {
                    writeln!(
                        self.iface,
                        "{}[{}{} {}{}{}] {}{}",
                        Fg(LightWhite),
                        Fg(Cyan),
                        datetime.format("%F %T"),
                        //datetime.format("%T"),
                        Fg(LightGreen),
                        smsbundle.src().unwrap(),
                        Fg(LightWhite),
                        termion::style::Reset,
                        message
                    )?;
                } else {
                    writeln!(
                        self.iface,
                        "{}[{}{} {}{} {}> {}{}{} ] {}{}",
                        Fg(LightWhite),
                        Fg(Cyan),
                        datetime.format("%F %T"),
                        //datetime.format("%T"),
                        Fg(LightGreen),
                        smsbundle.src().unwrap(),
                        Fg(LightWhite),
                        Fg(Green),
                        smsbundle.dst().unwrap(),
                        Fg(LightWhite),
                        termion::style::Reset,
                        message
                    )?;
                }
            } else {
                if self.verbose {
                    writeln!(self.iface, "{}Unexpected payload!{}", Fg(Red), style::Reset)?;
                }
            }
        }
        Ok(())
    }
}
impl Handler for ChatConnection {
    fn on_open(&mut self, _: Handshake) -> ws::Result<()> {
        writeln!(
            self.iface,
            "{}subscribing to {}{}",
            Fg(Yellow),
            self.localnode,
            style::Reset
        )?;
        self.out.send(format!("/subscribe {}", self.localnode))?;
        self.out.send("/bundle".to_string())?;
        Ok(())
    }

    fn on_message(&mut self, msg: Message) -> ws::Result<()> {
        match msg {
            Message::Text(txt) => {
                if txt == "subscribed" {
                    self.subscribed = true;
                    writeln!(self.iface, "subscribed")?;
                } else if txt.starts_with("200") {
                } else {
                    writeln!(
                        self.iface,
                        "{}Unexpected response: {}{}",
                        Fg(Red),
                        txt,
                        style::Reset
                    )?;
                    self.out.close(CloseCode::Error)?;
                }
            }
            Message::Binary(bin) => {
                let bndl: Bundle =
                    Bundle::try_from(bin).expect("Error decoding bundle from server");
                self.on_bundle(bndl).expect("Error handling bundle");
            }
        }
        Ok(())
    }
}
