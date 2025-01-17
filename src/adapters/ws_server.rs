use crate::message::Message;
use crate::actor::{Actor, Addr, ActorContext};
use crate::Config;
use crate::adapters::ws_conn::WsConn;
use crate::Node;

use async_trait::async_trait;
use std::collections::HashSet;
use std::sync::{Arc};
use std::fs::File;
use std::io::Read;
use tokio::sync::RwLock;
use tokio::time::{sleep, Duration};

use futures_util::{StreamExt, future};
use log::{info};
use tokio_native_tls::native_tls::{Identity};
use tokio::net::TcpListener;

use tokio_tungstenite::MaybeTlsStream;

use hyper::{Body, Request, Response};
use hyper::service::{make_service_fn, service_fn};
use hyper_staticfile::Static;
use std::io::Error as IoError;
use std::path::Path;

type Clients = Arc<RwLock<HashSet<Addr>>>;

pub struct WsServerConfig {
    pub port: u16,
    pub cert_path: Option<String>,
    pub key_path: Option<String>,
}
impl Default for WsServerConfig {
    fn default() -> Self {
        WsServerConfig {
            port: 4944,
            cert_path: None,
            key_path: None,
        }
    }
}

pub struct WsServer {
    config: Config,
    ws_config: WsServerConfig,
    clients: Clients
}
impl WsServer {
    pub fn new(config: Config) -> Self {
        Self::new_with_config(config, WsServerConfig::default())
    }

    pub fn new_with_config(config: Config, ws_config: WsServerConfig) -> Self {
        Self {
            config,
            ws_config,
            clients: Clients::default()
        }
    }

    async fn handle_stream(stream: MaybeTlsStream<tokio::net::TcpStream>, ctx: &ActorContext, clients: Clients, allow_public_space: bool) {
        let ws_stream = match tokio_tungstenite::accept_async(stream).await {
            Ok(s) => s,
            Err(_e) => {
                // suppress errors from receiving normal http requests
                // error!("Error during the websocket handshake occurred: {}", e);
                // try to handle normal http request?
                return;
            }
        };

        let (sender, receiver) = ws_stream.split();

        let conn = WsConn::new(sender, receiver, allow_public_space);
        let addr = ctx.start_actor(Box::new(conn));
        clients.write().await.insert(addr);
    }

    async fn handle_request<B>(req: Request<B>, iris: Static, stats: Static, peer_id: String) -> Result<Response<Body>, IoError> {
        let path = req.uri().path();
        if path.starts_with("/stats") {
            return stats.clone().serve(req).await
        } else if path.starts_with("/peer_id") {
            return Ok(Response::new(peer_id.into()))
        } else {
            iris.clone().serve(req).await
        }
    }

    async fn start_web_server(port: u16, peer_id: String) {
        let iris = Static::new(Path::new("./assets/iris"));
        let stats = Static::new(Path::new("./assets"));

        let make_service = make_service_fn(|_| {
            let iris = iris.clone();
            let stats = stats.clone();
            let peer_id = peer_id.clone();
            future::ok::<_, hyper::Error>(service_fn(move |req| Self::handle_request(req, iris.clone(), stats.clone(), peer_id.clone())))
        });

        let addr = ([0, 0, 0, 0], port).into();
        let server = hyper::Server::bind(&addr).serve(make_service);
        eprintln!("Iris UI:            http://{}/", addr);
        eprintln!("Stats:              http://{}/stats", addr);
        server.await.expect("Web server failed");
    }

    async fn update_stats(ctx: ActorContext, mut stats: Node) {
        stats.put("a".into());
        let mut conns: usize = 0;
        loop {
            sleep(Duration::from_millis(1000)).await;
            let conns_new = ctx.child_actor_count() - 1;
            if conns_new == conns {
                continue;
            }
            conns = conns_new;
            stats.get("ws_server_connections").put(conns.into());
        }
    }
}
#[async_trait]
impl Actor for WsServer {
    async fn handle(&mut self, msg: Message, _ctx: &ActorContext) {
        for conn in self.clients.read().await.iter() {
            if msg.is_from(conn) {
                continue;
            }
            if let Err(_) = conn.send(msg.clone()) {
                self.clients.write().await.remove(conn);
            }
        }
    }

    async fn pre_start(&mut self, ctx: &ActorContext) {
        let addr = format!("0.0.0.0:{}", self.ws_config.port).to_string();
        let ctx = ctx.clone();

        let web_port = self.ws_config.port + 1;
        let peer_id = ctx.peer_id.read().unwrap().clone();
        let peer_id_clone = peer_id.clone();
        ctx.child_task(async move {
            Self::start_web_server(web_port, peer_id_clone).await;
        });

        if self.config.stats {
            let mut node = ctx.node.as_ref().unwrap().clone();
            let stats = node.get("node_stats").get(&peer_id);
            let ctx_clone = ctx.clone();
            ctx.child_task(async move {
                Self::update_stats(ctx_clone, stats).await;
            });
        }

        // Create the event loop and TCP listener we'll accept connections on.
        let try_socket = TcpListener::bind(&addr).await;
        let listener = try_socket.expect("Failed to bind");
        eprintln!("Websocket endpoint: ws://{}/ws", addr);

        let allow_public_space = self.config.allow_public_space;
        let clients = self.clients.clone();
        if let Some(cert_path) = &self.ws_config.cert_path {
            let mut cert_file = File::open(cert_path).unwrap();
            let mut cert = vec![];
            cert_file.read_to_end(&mut cert).unwrap();

            let key_path = self.ws_config.key_path.as_ref().unwrap();
            let mut key_file = File::open(key_path).unwrap();
            let mut key = vec![];
            key_file.read_to_end(&mut key).unwrap();

            let identity = Identity::from_pkcs8(&cert, &key).unwrap();
            let acceptor = tokio_native_tls::native_tls::TlsAcceptor::new(identity).unwrap();
            let acceptor = tokio_native_tls::TlsAcceptor::from(acceptor);
            let acceptor = Arc::new(acceptor);

            ctx.clone().child_task(async move {
                loop {
                    if let Ok((stream, _)) = listener.accept().await {
                        let acceptor = acceptor.clone();
                        let clients = clients.clone();
                        let ctx = ctx.clone();
                        tokio::spawn(async move {
                            let stream = acceptor.accept(stream).await;
                            match stream {
                                Ok(stream) => {
                                    Self::handle_stream(MaybeTlsStream::NativeTls(stream), &ctx, clients.clone(), allow_public_space).await;
                                },
                                _ => {}
                            }
                        });
                    }
                }
            });
        } else {
            ctx.clone().child_task(async move {
                while let Ok((stream, _)) = listener.accept().await {
                    Self::handle_stream(MaybeTlsStream::Plain(stream), &ctx, clients.clone(), allow_public_space).await;
                }
            });
        }
    }

    async fn stopping(&mut self, _context: &ActorContext) {
        info!("WsServer stopping");
    }
}
