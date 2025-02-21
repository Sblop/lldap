use crate::{
    domain::{
        handler::{BackendHandler, LoginHandler},
        opaque_handler::OpaqueHandler,
    },
    infra::{configuration::Configuration, ldap_handler::LdapHandler},
};
use actix_rt::net::TcpStream;
use actix_server::ServerBuilder;
use actix_service::{fn_service, ServiceFactoryExt};
use anyhow::{Context, Result};
use futures_util::future::ok;
use ldap3_server::{proto::LdapMsg, LdapCodec};
use log::*;
use tokio::net::tcp::WriteHalf;
use tokio_util::codec::{FramedRead, FramedWrite};

async fn handle_incoming_message<Backend>(
    msg: Result<LdapMsg, std::io::Error>,
    resp: &mut FramedWrite<WriteHalf<'_>, LdapCodec>,
    session: &mut LdapHandler<Backend>,
) -> Result<bool>
where
    Backend: BackendHandler + LoginHandler + OpaqueHandler,
{
    use futures_util::SinkExt;
    let msg = msg.context("while receiving LDAP op")?;
    debug!("Received LDAP message: {:?}", &msg);
    match session.handle_ldap_message(msg.op).await {
        None => return Ok(false),
        Some(result) => {
            if result.is_empty() {
                debug!("No response");
            }
            for result_op in result.into_iter() {
                debug!("Replying with LDAP op: {:?}", &result_op);
                resp.send(LdapMsg {
                    msgid: msg.msgid,
                    op: result_op,
                    ctrl: vec![],
                })
                .await
                .context("while sending a response: {:#}")?
            }

            resp.flush()
                .await
                .context("while flushing responses: {:#}")?
        }
    }
    Ok(true)
}

pub fn build_ldap_server<Backend>(
    config: &Configuration,
    backend_handler: Backend,
    server_builder: ServerBuilder,
) -> Result<ServerBuilder>
where
    Backend: BackendHandler + LoginHandler + OpaqueHandler + 'static,
{
    use futures_util::StreamExt;

    let ldap_base_dn = config.ldap_base_dn.clone();
    let ldap_user_dn = config.ldap_user_dn.clone();
    server_builder
        .bind("ldap", ("0.0.0.0", config.ldap_port), move || {
            let backend_handler = backend_handler.clone();
            let ldap_base_dn = ldap_base_dn.clone();
            let ldap_user_dn = ldap_user_dn.clone();
            fn_service(move |mut stream: TcpStream| {
                let backend_handler = backend_handler.clone();
                let ldap_base_dn = ldap_base_dn.clone();
                let ldap_user_dn = ldap_user_dn.clone();
                async move {
                    // Configure the codec etc.
                    let (r, w) = stream.split();
                    let mut requests = FramedRead::new(r, LdapCodec);
                    let mut resp = FramedWrite::new(w, LdapCodec);

                    let mut session = LdapHandler::new(backend_handler, ldap_base_dn, ldap_user_dn);

                    while let Some(msg) = requests.next().await {
                        if !handle_incoming_message(msg, &mut resp, &mut session)
                            .await
                            .context("while handling incoming messages")?
                        {
                            break;
                        }
                    }

                    Ok(stream)
                }
            })
            .map_err(|err: anyhow::Error| error!("Service Error: {:#}", err))
            .and_then(move |_| {
                // finally
                ok(())
            })
        })
        .with_context(|| format!("while binding to the port {}", config.ldap_port))
}
