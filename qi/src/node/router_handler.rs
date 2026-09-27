use crate::{
    error::{HandlerError, NoHandlerError},
    messaging::{self, message},
    object::{self, BoxObject, HandlerExt},
    service, Error,
};
use bytes::Bytes;
use futures::{future::BoxFuture, FutureExt};
use qi_value::{ActionId, KeyDynValueMap};
use std::collections::{hash_map, HashMap};
use std::sync::Arc;
use tokio::{sync::Mutex, task};
use tracing::info;

#[derive(Default)]
pub(super) struct RouterHandler {
    handlers: HashMap<service::Id, ServiceHandler>,
}

impl std::fmt::Debug for RouterHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RouterHandler")
            .field("handlers", &self.handlers)
            .finish()
    }
}

impl RouterHandler {
    pub(super) fn insert(&mut self, name: String, info: service::Info, main_object: BoxObject) {
        self.handlers
            .insert(info.id(), ServiceHandler::new(name, info, main_object));
    }

    pub(super) fn info_mut(&mut self) -> impl Iterator<Item = &mut service::Info> {
        self.handlers.values_mut().map(|data| &mut data.info)
    }

    pub(super) async fn call(
        &mut self,
        address: message::Address,
        args: Bytes,
    ) -> Result<Bytes, Error> {
        let (object, ident) = self
            .try_get_request_handler(address)
            .ok_or(NoHandlerError(message::Type::Call, address))?;
        object.handler_meta_call(ident, args).await
    }

    pub(super) async fn post(&mut self, address: message::Address, args: Bytes) {
        let (object, action) = match self.try_get_request_handler(address) {
            Some(handler) => handler,
            None => {
                info!(%address, "post request discarded: no handler");
                return;
            }
        };
        object.handler_meta_post(action, args).await
    }

    pub(super) async fn event(&mut self, address: message::Address, args: Bytes) {
        let (object, action) = match self.try_get_request_handler(address) {
            Some(handler) => handler,
            None => {
                info!(%address, "event request discarded: no handler");
                return;
            }
        };
        object.handler_meta_event(action, args).await
    }

    fn try_get_request_handler(&self, address: message::Address) -> Option<(&BoxObject, ActionId)> {
        let message::Address(service_id, object_id, action_id) = address;
        let object = self
            .handlers
            .get(&service_id)
            .and_then(|data| data.bound_objects.get(&object_id))?;
        Some((object, action_id))
    }
}

#[derive(Default)]
pub(super) struct ServiceHandler {
    name: String,
    info: service::Info,
    bound_objects: HashMap<object::Id, BoxObject>,
}

impl ServiceHandler {
    pub(super) fn new(name: String, info: service::Info, main_object: BoxObject) -> Self {
        Self {
            name,
            info,
            bound_objects: [(service::MAIN_OBJECT_ID, main_object)]
                .into_iter()
                .collect(),
        }
    }
}

impl std::fmt::Debug for ServiceHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServiceData")
            .field("name", &self.name)
            .field("info", &self.info)
            .field("bound_objects", &self.bound_objects.keys())
            .finish()
    }
}

#[derive(Default)]
pub(super) struct PendingServiceMap(HashMap<String, BoxObject>);

impl PendingServiceMap {
    pub(super) fn add(&mut self, name: String, object: BoxObject) {
        self.0.insert(name, object);
    }
}

impl std::fmt::Debug for PendingServiceMap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.0.keys()).finish()
    }
}

impl IntoIterator for PendingServiceMap {
    type Item = (String, BoxObject);
    type IntoIter = hash_map::IntoIter<String, BoxObject>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

pub(super) struct ArcRouterHandler(Arc<Mutex<RouterHandler>>);

impl Clone for ArcRouterHandler {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl std::fmt::Debug for ArcRouterHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl ArcRouterHandler {
    pub(crate) fn new(services: Arc<Mutex<RouterHandler>>) -> Self {
        Self(services)
    }
}

impl messaging::CallHandler for ArcRouterHandler {
    type Error = HandlerError;
    type Future = BoxFuture<'static, Result<Bytes, Self::Error>>;

    fn handle_call(&self, address: message::Address, value: Bytes) -> Self::Future {
        let router = Arc::clone(&self.0);
        async move {
            router
                .lock_owned()
                .await
                .call(address, value)
                .await
                .map_err(HandlerError::non_fatal)
        }
        .boxed()
    }
}

impl messaging::EventHandler for ArcRouterHandler {
    fn handle_event(&self, address: message::Address, value: Bytes) {
        // The messaging loop handles events without awaiting them, so handling is spawned.
        let router = Arc::clone(&self.0);
        task::spawn(async move { router.lock_owned().await.event(address, value).await });
    }
}

impl messaging::PostHandler for ArcRouterHandler {
    fn handle_post(&self, address: message::Address, value: Bytes) {
        // The messaging loop handles posts without awaiting them, so handling is spawned.
        let router = Arc::clone(&self.0);
        task::spawn(async move { router.lock_owned().await.post(address, value).await });
    }
}

impl messaging::CapabilitiesHandler for ArcRouterHandler {
    fn handle_capabilities(&self, _address: message::Address, _data: KeyDynValueMap) {
        // Capabilities messages are not handled by nodes services and messaging handler.
    }
}
