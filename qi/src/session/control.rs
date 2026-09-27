use super::{
    auth::{self, Authenticator},
    capabilities,
};
use crate::{
    error::{FormatError, HandlerError, NoHandlerError},
    format, messaging,
    value::{ActionId, KeyDynValueMap, ObjectId, ServiceId},
    Error,
};
use bytes::Bytes;
use futures::{future::BoxFuture, FutureExt};
use messaging::message;
use std::sync::Arc;
use tokio::sync::watch;

const SERVICE_ID: ServiceId = ServiceId(0);
const OBJECT_ID: ObjectId = ObjectId(0);
const AUTHENTICATE_ACTION_ID: ActionId = ActionId(8);

fn is_control_address(address: message::Address) -> bool {
    address.service() == SERVICE_ID && address.object() == OBJECT_ID
}

pub(crate) const AUTHENTICATE_ADDRESS: message::Address =
    message::Address(SERVICE_ID, OBJECT_ID, AUTHENTICATE_ACTION_ID);

#[derive(Clone)]
pub(super) struct Controller {
    authenticator: Arc<dyn Authenticator + Send + Sync>,
    capabilities: watch::Sender<Option<KeyDynValueMap>>,
    remote_authorized: watch::Sender<bool>,
}

impl Controller {
    fn authenticate(
        &self,
        request: KeyDynValueMap,
    ) -> Result<KeyDynValueMap, AuthenticateClientError> {
        let shared_capabilities = capabilities::shared_with_local(&request);
        capabilities::check_required(&shared_capabilities)?;
        self.authenticator
            .verify(request)
            .map_err(AuthenticateClientError::AuthenticationVerification)?;
        self.capabilities
            .send_replace(Some(shared_capabilities.clone()));
        self.remote_authorized.send_replace(true);
        Ok(auth::state_done_map(shared_capabilities))
    }

    pub(super) async fn authenticate_to_server(
        &self,
        client: &messaging::Client,
        parameters: KeyDynValueMap,
    ) -> Result<(), Error> {
        // Reset the current capabilities
        self.capabilities.send_replace(None);
        let mut request = capabilities::local_map().clone();
        request.extend(parameters);
        let authenticate_result = client
            .call(
                AUTHENTICATE_ADDRESS,
                format::to_bytes(&request).map_err(FormatError::ArgumentsSerialization)?,
            )
            .await?;
        let mut shared_capabilities = format::from_slice(&authenticate_result)
            .map_err(FormatError::MethodReturnValueDeserialization)?;
        auth::extract_state_result(&mut shared_capabilities)
            .map_err(AuthenticateToServerError::ResultState)?;
        capabilities::check_required(&shared_capabilities)
            .map_err(AuthenticateToServerError::UnexpectedServerCapabilityValue)?;
        self.capabilities.send_replace(Some(shared_capabilities));
        Ok(())
    }
}

impl std::fmt::Debug for Controller {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Control")
            .field("capabilities", &self.capabilities)
            .field("remote_authorized", &self.remote_authorized)
            .finish()
    }
}

/// The messaging handler contract that sessions require from the handler they drive.
///
/// On top of being a messaging handler that can be shared between tasks, call errors must be
/// `HandlerError` so that the control layer can report cancellation and fatality conditions to the
/// messaging loop.
pub(crate) trait SessionHandler:
    messaging::Handler + messaging::CallHandler<Error = HandlerError> + Send + Sync + 'static
{
}

impl<H> SessionHandler for H where
    H: messaging::Handler + messaging::CallHandler<Error = HandlerError> + Send + Sync + 'static
{
}

pub(super) struct Control<H> {
    pub(super) controller: Controller,
    pub(super) capabilities: watch::Receiver<Option<KeyDynValueMap>>,
    pub(super) remote_authorized: watch::Receiver<bool>,
    pub(super) handler: ControlledHandler<H>,
}

pub(super) fn make<Handler, Auth>(
    handler: Handler,
    authenticator: Auth,
    remote_authorized: bool,
) -> Control<Handler>
where
    Handler: SessionHandler,
    Auth: Authenticator + Send + Sync + 'static,
{
    let (capabilities_sender, capabilities_receiver) = watch::channel(Default::default());
    let (remote_authorized_sender, remote_authorized_receiver) = watch::channel(remote_authorized);
    let controller = Controller {
        authenticator: Arc::new(authenticator),
        capabilities: capabilities_sender,
        remote_authorized: remote_authorized_sender,
    };
    let controlled_handler = ControlledHandler {
        inner: handler,
        controller: controller.clone(),
    };
    Control {
        controller,
        capabilities: capabilities_receiver,
        remote_authorized: remote_authorized_receiver,
        handler: controlled_handler,
    }
}

pub(super) struct ControlledHandler<H> {
    inner: H,
    controller: Controller,
}

impl<Handler> messaging::CallHandler for ControlledHandler<Handler>
where
    Handler: SessionHandler + Clone,
{
    type Error = HandlerError;
    type Future = BoxFuture<'static, Result<Bytes, Self::Error>>;

    fn handle_call(&self, address: message::Address, args: Bytes) -> Self::Future {
        // The call future must be independent of the handler borrow, so the state it needs is
        // cloned into the future.
        let controller = self.controller.clone();
        let inner = self.inner.clone();
        async move {
            if is_control_address(address) {
                let request = format::from_slice(&args)
                    .map_err(FormatError::ArgumentsDeserialization)
                    .map_err(HandlerError::non_fatal)?;
                let result = controller
                    .authenticate(request)
                    // All authentication errors are fatal
                    .map_err(HandlerError::fatal)?;
                format::to_bytes(&result)
                    .map_err(FormatError::MethodReturnValueSerialization)
                    .map_err(HandlerError::non_fatal)
            } else if *controller.remote_authorized.borrow() {
                inner.handle_call(address, args).await
            } else {
                Err(HandlerError::non_fatal(NoHandlerError(
                    message::Type::Call,
                    address,
                )))
            }
        }
        .boxed()
    }
}

impl<Handler> messaging::EventHandler for ControlledHandler<Handler>
where
    Handler: SessionHandler,
{
    fn handle_event(&self, address: message::Address, args: Bytes) {
        if is_control_address(address) {
            // TODO: Handle capabilities request ?
        } else if *self.controller.remote_authorized.borrow() {
            self.inner.handle_event(address, args)
        }
    }
}

impl<Handler> messaging::PostHandler for ControlledHandler<Handler>
where
    Handler: SessionHandler,
{
    fn handle_post(&self, address: message::Address, args: Bytes) {
        if is_control_address(address) {
            // TODO: Handle capabilities request ?
        } else if *self.controller.remote_authorized.borrow() {
            self.inner.handle_post(address, args)
        }
    }
}

impl<Handler> messaging::CapabilitiesHandler for ControlledHandler<Handler>
where
    Handler: SessionHandler,
{
    fn handle_capabilities(&self, address: message::Address, data: KeyDynValueMap) {
        if is_control_address(address) {
            // TODO: Handle capabilities request ?
        } else if *self.controller.remote_authorized.borrow() {
            self.inner.handle_capabilities(address, data)
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub(super) enum AuthenticateClientError {
    #[error("unexpected capability value")]
    UnexpectedclientCapabilityValue(#[from] capabilities::KeyValueExpectError),

    #[error("failure to verify authentication request")]
    AuthenticationVerification(#[source] auth::Error),
}

impl From<AuthenticateClientError> for Error {
    fn from(err: AuthenticateClientError) -> Self {
        Error::Other(err.into())
    }
}

#[derive(Debug, thiserror::Error)]
pub(super) enum AuthenticateToServerError {
    #[error("the authentication state sent back by the server is invalid")]
    ResultState(#[from] auth::StateError),

    #[error("the server sent an unexpected capability value")]
    UnexpectedServerCapabilityValue(#[from] capabilities::KeyValueExpectError),
}

impl From<AuthenticateToServerError> for Error {
    fn from(err: AuthenticateToServerError) -> Self {
        Error::Other(err.into())
    }
}
