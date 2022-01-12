use actix::{Actor, StreamHandler, AsyncContext, Handler};
use actix_web::{web, App, Error, HttpRequest, HttpResponse, HttpServer};
use actix_web_actors::ws;
use std::env;
use async_trait::async_trait;
use crate::types::{NetworkAdapter, GunMessage};
use crate::Node;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT_USER_ID: AtomicUsize = AtomicUsize::new(1);

/// Define HTTP actor
pub struct MyWs {
    node: Node,
    id: String
}

struct OutgoingMessage {
    gun_message: GunMessage
}

impl actix::Message for OutgoingMessage {
    type Result = ();
}

impl Actor for MyWs {
    type Context = ws::WebsocketContext<Self>;

    fn started(&mut self, ctx: &mut ws::WebsocketContext<Self>) {
        let mut rx = self.node.get_outgoing_msg_receiver();
        let id = self.id.clone();
        let addr = ctx.address();
        tokio::task::spawn(async move {
            loop {
                if let Ok(message) = rx.recv().await {
                    if message.from == id {
                        continue;
                    }
                    let res = addr.try_send(OutgoingMessage { gun_message: message }); // TODO break on error
                    if let Err(e) = res {
                        break;
                    }
                }
            }
        });
    }
}

impl Handler<OutgoingMessage> for MyWs {
    type Result = ();

    fn handle(&mut self, msg: OutgoingMessage, ctx: &mut Self::Context) {
        let text = format!("{}", msg.gun_message.msg);
        ctx.text(text);
    }
}

/// Handler for ws::Message message
impl StreamHandler<Result<ws::Message, ws::ProtocolError>> for MyWs {
    fn handle(
        &mut self,
        msg: Result<ws::Message, ws::ProtocolError>,
        ctx: &mut Self::Context,
    ) {
        match msg {
            Ok(ws::Message::Ping(msg)) => ctx.pong(&msg),
            Ok(ws::Message::Text(text)) => self.node.incoming_message(text.to_string(), &self.id),
            _ => (),
        }
    }
}

pub struct WebsocketServer {
    node: Node
}

#[async_trait]
impl NetworkAdapter for WebsocketServer {
    fn new(node: Node) -> Self {
        WebsocketServer {
            node
        }
    }

    async fn start(&self) {
        let node = self.node.clone();
        tokio::task::spawn_blocking(move || { // spawn_blocking - problem for NetworkAdapter?
            Self::actix_start(node);
        });
    }

    fn stop(&self) {

    }
}

impl WebsocketServer {
    #[actix_web::main]
    async fn actix_start(node: Node) -> std::io::Result<()> {
        let port: u16 = match env::var("PORT") {
            Ok(p) => p.parse::<u16>().unwrap(),
            _ => 4944
        };

        HttpServer::new(move || {
            let node = node.clone();
            App::new().route("/gun", web::get().to(
                move |a, b| {
                    Self::user_connected(a, b, node.clone())
                }
            ))
        })
            .bind(format!("0.0.0.0:{}", port)).unwrap()
            .run()
            .await
    }

    async fn user_connected(req: HttpRequest, stream: web::Payload, node: Node) -> Result<HttpResponse, Error> {
        // Use a counter to assign a new unique ID for this user.
        let id = NEXT_USER_ID.fetch_add(1, Ordering::Relaxed);
        let id = format!("ws_server_{}", id).to_string();

        let ws = MyWs { node, id };
        let resp = ws::start(ws, &req, stream);

        //println!("{:?}", resp);
        resp
    }
}
