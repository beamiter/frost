//! Read-only Settings-portal observation. The shared core owns motion policy.
//!
//! No periodic polling or reconnection timer is used. A disconnected session bus
//! fails closed and parks until the UI starts a fresh subscription epoch. Portal
//! owner changes on a healthy bus are observed and trigger one bounded read.

use iced::futures::{channel::mpsc, SinkExt};
use jterm_core::organism_motion::SystemMotion;

/// A bounded diagnostic vocabulary; never contains bus errors or payload text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Initializing,
    Portal,
    NoService,
    MissingKey,
    InvalidType,
    Denied,
    Timeout,
    Disconnected,
    Unavailable,
    IdentityExhausted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Event {
    pub subscription_epoch: u64,
    pub sequence: u64,
    pub observed: SystemMotion,
    pub source: Source,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Fact {
    observed: SystemMotion,
    source: Source,
}

impl Fact {
    fn unavailable(source: Source) -> Self {
        Self {
            observed: SystemMotion::Unavailable,
            source,
        }
    }
}

/// Identity belongs to one Automatic activation, never a render/frame.
pub fn subscription(subscription_epoch: u64) -> iced::Subscription<Event> {
    iced::Subscription::run_with(subscription_epoch, |epoch| {
        let epoch = *epoch;
        iced::stream::channel(4, move |mut output| async move {
            let mut emitter = Emitter::new(epoch);
            if !emitter
                .send(&mut output, Fact::unavailable(Source::Initializing))
                .await
            {
                return;
            }
            #[cfg(target_os = "linux")]
            let stopped = native::watch(&mut output, &mut emitter).await;
            #[cfg(not(target_os = "linux"))]
            let stopped = Source::Unavailable;
            if emitter.send(&mut output, Fact::unavailable(stopped)).await {
                // Keep the subscription alive without a timer, socket or retry.
                iced::futures::future::pending::<()>().await;
            }
        })
    })
}

struct Emitter {
    epoch: u64,
    sequence: u64,
    last: Option<Fact>,
}

impl Emitter {
    fn new(epoch: u64) -> Self {
        Self {
            epoch,
            sequence: 0,
            last: None,
        }
    }

    fn event(&mut self, mut fact: Fact) -> Option<Event> {
        if self.last == Some(fact) || self.sequence == u64::MAX {
            return None;
        }
        self.sequence += 1;
        if self.sequence == u64::MAX {
            // Reserve the last identity to clear any previously observed Full.
            fact = Fact::unavailable(Source::IdentityExhausted);
        }
        self.last = Some(fact);
        Some(Event {
            subscription_epoch: self.epoch,
            sequence: self.sequence,
            observed: fact.observed,
            source: fact.source,
        })
    }

    async fn send(&mut self, output: &mut mpsc::Sender<Event>, fact: Fact) -> bool {
        if let Some(event) = self.event(fact) {
            if output.send(event).await.is_err() {
                return false;
            }
        }
        self.sequence != u64::MAX
    }
}

#[cfg(target_os = "linux")]
mod native {
    use super::{Emitter, Event, Fact, Source};
    use crate::organism_motion_observation::{
        decode_portal_motion, Observation, ProviderEpoch, ReadTicket,
    };
    use iced::futures::{
        channel::mpsc,
        future::{self, BoxFuture, Fuse},
        FutureExt, StreamExt,
    };
    use std::time::Duration;
    use zbus::{
        message::Type,
        names::OwnedUniqueName,
        zvariant::{Structure, Value},
        Connection, MatchRule, Message, MessageStream,
    };

    const SERVICE: &str = "org.freedesktop.portal.Desktop";
    const PATH: &str = "/org/freedesktop/portal/desktop";
    const SETTINGS: &str = "org.freedesktop.portal.Settings";
    const NAMESPACE: &str = "org.freedesktop.appearance";
    const KEY: &str = "reduced-motion";
    const BUS: &str = "org.freedesktop.DBus";
    const BUS_PATH: &str = "/org/freedesktop/DBus";
    const BOUND: Duration = Duration::from_secs(3);
    const BUS_QUEUE: usize = 32;

    enum Work {
        Owner(Result<Option<OwnedUniqueName>, Source>),
        Read(ReadTicket, Fact),
    }

    struct Pending {
        operation: Fuse<BoxFuture<'static, Work>>,
        deadline: Fuse<BoxFuture<'static, ()>>,
        ticket: Option<ReadTicket>,
    }

    impl Pending {
        fn bounded(operation: BoxFuture<'static, Work>, ticket: Option<ReadTicket>) -> Self {
            Self {
                operation: operation.fuse(),
                deadline: tokio::time::sleep(BOUND).boxed().fuse(),
                ticket,
            }
        }
    }

    fn idle() -> Pending {
        Pending {
            operation: future::pending().boxed().fuse(),
            deadline: future::pending().boxed().fuse(),
            ticket: None,
        }
    }

    struct Provider {
        owner: Option<OwnedUniqueName>,
        epoch: Option<ProviderEpoch>,
        signals: u64,
        observation: Observation,
    }

    impl Provider {
        fn new() -> Self {
            Self {
                owner: None,
                epoch: None,
                signals: 0,
                observation: Observation::default(),
            }
        }

        fn replace(&mut self, owner: Option<OwnedUniqueName>) -> Result<(), Source> {
            if let Some(epoch) = self.epoch.take() {
                self.observation.disconnect(epoch);
            }
            self.owner = owner;
            self.signals = 0;
            if self.owner.is_some() {
                self.epoch = Some(
                    self.observation
                        .begin_provider()
                        .ok_or(Source::IdentityExhausted)?,
                );
            }
            Ok(())
        }
    }

    pub(super) async fn watch(output: &mut mpsc::Sender<Event>, emitter: &mut Emitter) -> Source {
        match run(output, emitter).await {
            Ok(()) => Source::Unavailable,
            Err(source) => source,
        }
    }

    async fn run(output: &mut mpsc::Sender<Event>, emitter: &mut Emitter) -> Result<(), Source> {
        let connection = tokio::time::timeout(BOUND, async {
            zbus::connection::Builder::session()?
                .max_queued(BUS_QUEUE)
                .build()
                .await
        })
        .await
        .map_err(|_| Source::Timeout)?
        .map_err(|error| error_source(&error))?;
        // This connection is exclusively ours. The bus receives only the two
        // narrow match rules below; the single queue preserves owner/signal order.
        let mut messages = MessageStream::from(&connection);
        let closed = connection.closed().fuse();
        iced::futures::pin_mut!(closed);
        {
            let setup = tokio::time::timeout(BOUND, subscribe(&connection)).fuse();
            iced::futures::pin_mut!(setup);
            loop {
                iced::futures::select_biased! {
                    _ = closed => return Err(Source::Disconnected),
                    result = setup => {
                        result.map_err(|_| Source::Timeout)??;
                        break;
                    },
                    message = messages.next().fuse() => {
                        // No read has started. A fresh owner query and read follow
                        // registration, so pre-registration signals need no cache.
                        if !matches!(message, Some(Ok(_))) {
                            return Err(Source::Disconnected);
                        }
                    },
                }
            }
        }

        let mut provider = Provider::new();
        let mut work = query_owner(connection.clone());
        loop {
            // Process queued owner/signal messages before a ready read result.
            // A read also carries an epoch/read-ticket/signal sequence, so it
            // cannot restore an obsolete value after either kind of change.
            let completed = iced::futures::select_biased! {
                _ = closed => return Err(Source::Disconnected),
                _ = &mut work.deadline => {
                    // Deadline precedes the message branch: even continuous
                    // unrelated/untrusted traffic cannot starve the read bound.
                    let result = match work.ticket {
                        Some(ticket) => Work::Read(ticket, Fact::unavailable(Source::Timeout)),
                        None => Work::Owner(Err(Source::Timeout)),
                    };
                    work = idle();
                    Some(result)
                },
                message = messages.next().fuse() => {
                    let message = match message {
                        Some(Ok(message)) => message,
                        _ => return Err(Source::Disconnected),
                    };
                    if let Some(owner) = owner_change(&message) {
                        Some(Work::Owner(owner))
                    } else {
                        if let (Some(owner), Some(epoch)) = (&provider.owner, provider.epoch) {
                            if let Some(fact) = setting_change(&message, owner.as_str()) {
                                provider.signals = provider.signals.checked_add(1)
                                    .ok_or(Source::IdentityExhausted)?;
                                if provider.observation.signal(epoch, provider.signals, fact.observed) {
                                    // Cancellation prevents a stale fallback read as well.
                                    work = idle();
                                    let fact = Fact { observed: provider.observation.value(), ..fact };
                                    if !emitter.send(output, fact).await {
                                        return Ok(());
                                    }
                                }
                            }
                        }
                        None
                    }
                },
                completed = &mut work.operation => {
                    work = idle();
                    Some(completed)
                },
            };
            match completed {
                Some(Work::Owner(Ok(owner))) => {
                    if owner == provider.owner && provider.epoch.is_some() {
                        continue;
                    }
                    provider.replace(owner)?;
                    work = idle();
                    let source = if provider.owner.is_some() {
                        Source::Initializing
                    } else {
                        Source::NoService
                    };
                    if !emitter.send(output, Fact::unavailable(source)).await {
                        return Ok(());
                    }
                    if let (Some(owner), Some(epoch)) = (&provider.owner, provider.epoch) {
                        let ticket = provider
                            .observation
                            .begin_read(epoch)
                            .ok_or(Source::IdentityExhausted)?;
                        work = read_motion(connection.clone(), owner.clone(), ticket);
                    }
                }
                Some(Work::Owner(Err(source))) => {
                    provider.replace(None)?;
                    work = idle();
                    if !emitter.send(output, Fact::unavailable(source)).await {
                        return Ok(());
                    }
                }
                Some(Work::Read(ticket, fact))
                    if provider.observation.complete_read(ticket, fact.observed)
                        && !emitter.send(output, fact).await =>
                {
                    return Ok(());
                }
                Some(Work::Read(_, _)) | None => {}
            }
        }
    }

    async fn subscribe(connection: &Connection) -> Result<(), Source> {
        let rules = [
            MatchRule::builder()
                .msg_type(Type::Signal)
                .sender(BUS)
                .and_then(|rule| rule.path(BUS_PATH))
                .and_then(|rule| rule.interface(BUS))
                .and_then(|rule| rule.member("NameOwnerChanged"))
                .and_then(|rule| rule.add_arg(SERVICE)),
            MatchRule::builder()
                .msg_type(Type::Signal)
                .sender(SERVICE)
                .and_then(|rule| rule.path(PATH))
                .and_then(|rule| rule.interface(SETTINGS))
                .and_then(|rule| rule.member("SettingChanged"))
                .and_then(|rule| rule.add_arg(NAMESPACE))
                .and_then(|rule| rule.add_arg(KEY)),
        ];
        for rule in rules {
            let rule = rule.map_err(|_| Source::Unavailable)?.build();
            connection
                .call_method(
                    Some(BUS),
                    BUS_PATH,
                    Some(BUS),
                    "AddMatch",
                    &(rule.to_string(),),
                )
                .await
                .map_err(|error| error_source(&error))?;
        }
        // Rules are connection-scoped; dropping this dedicated connection also
        // retires its registrations. There is no separately spawned listener.
        Ok(())
    }

    fn query_owner(connection: Connection) -> Pending {
        Pending::bounded(
            async move {
                let result = async {
                    let reply = connection
                        .call_method(Some(BUS), BUS_PATH, Some(BUS), "GetNameOwner", &(SERVICE,))
                        .await
                        .map_err(|error| error_source(&error))?;
                    if reply.header().sender().map(|sender| sender.as_str()) != Some(BUS) {
                        return Err(Source::Unavailable);
                    }
                    reply
                        .body()
                        .deserialize::<OwnedUniqueName>()
                        .map(Some)
                        .map_err(|_| Source::InvalidType)
                }
                .await;
                Work::Owner(result)
            }
            .boxed(),
            None,
        )
    }

    fn read_motion(connection: Connection, owner: OwnedUniqueName, ticket: ReadTicket) -> Pending {
        Pending::bounded(
            async move {
                let fact = read_preference(&connection, owner.as_str()).await;
                Work::Read(ticket, fact)
            }
            .boxed(),
            Some(ticket),
        )
    }

    async fn read_preference(connection: &Connection, owner: &str) -> Fact {
        let first = connection
            .call_method(
                Some(owner),
                PATH,
                Some(SETTINGS),
                "ReadOne",
                &(NAMESPACE, KEY),
            )
            .await;
        let (reply, legacy) = match first {
            Ok(reply) => (Ok(reply), false),
            Err(error) if unknown_method_from(&error, owner) => (
                connection
                    .call_method(Some(owner), PATH, Some(SETTINGS), "Read", &(NAMESPACE, KEY))
                    .await,
                true,
            ),
            Err(error) => return Fact::unavailable(error_source(&error)),
        };
        match reply {
            Ok(reply) => decode_reply(&reply, owner, legacy),
            Err(error) => Fact::unavailable(error_source(&error)),
        }
    }

    fn decode_reply(reply: &Message, owner: &str, legacy: bool) -> Fact {
        if reply.message_type() != Type::MethodReturn
            || reply.header().sender().map(|sender| sender.as_str()) != Some(owner)
        {
            return Fact::unavailable(Source::Unavailable);
        }
        let body = reply.body();
        match body.deserialize::<Value<'_>>() {
            Ok(value) => decode_value(&value, legacy),
            Err(_) => Fact::unavailable(Source::InvalidType),
        }
    }

    fn decode_value(value: &Value<'_>, legacy: bool) -> Fact {
        let value = if legacy {
            match value {
                Value::Value(inner) => inner.as_ref(),
                _ => return Fact::unavailable(Source::InvalidType),
            }
        } else {
            value
        };
        match value {
            Value::U32(value) => Fact {
                observed: decode_portal_motion(Some(*value)),
                source: Source::Portal,
            },
            _ => Fact::unavailable(Source::InvalidType),
        }
    }

    fn signal_header(
        message: &Message,
        sender: &str,
        path: &str,
        interface: &str,
        member: &str,
    ) -> bool {
        let header = message.header();
        message.message_type() == Type::Signal
            && header.sender().map(|value| value.as_str()) == Some(sender)
            && header.path().map(|value| value.as_str()) == Some(path)
            && header.interface().map(|value| value.as_str()) == Some(interface)
            && header.member().map(|value| value.as_str()) == Some(member)
    }

    fn owner_change(message: &Message) -> Option<Result<Option<OwnedUniqueName>, Source>> {
        if !signal_header(message, BUS, BUS_PATH, BUS, "NameOwnerChanged") {
            return None;
        }
        let body = message.body();
        let (name, _old, new) = body.deserialize::<(&str, &str, &str)>().ok()?;
        if name != SERVICE {
            return None;
        }
        Some(if new.is_empty() {
            Ok(None)
        } else {
            OwnedUniqueName::try_from(new.to_owned())
                .map(Some)
                .map_err(|_| Source::InvalidType)
        })
    }

    fn setting_change(message: &Message, owner: &str) -> Option<Fact> {
        if !signal_header(message, owner, PATH, SETTINGS, "SettingChanged") {
            return None;
        }
        let body = message.body();
        let fields = body.deserialize::<Structure<'_>>().ok()?;
        let values = fields.fields();
        if !matches!(values.first(), Some(Value::Str(value)) if value.as_str() == NAMESPACE)
            || !matches!(values.get(1), Some(Value::Str(value)) if value.as_str() == KEY)
        {
            return None;
        }
        Some(match values {
            [_, _, Value::Value(value)] => decode_value(value, false),
            _ => Fact::unavailable(Source::InvalidType),
        })
    }

    fn unknown_method_from(error: &zbus::Error, owner: &str) -> bool {
        matches!(error, zbus::Error::MethodError(name, _, reply)
            if name.as_str() == "org.freedesktop.DBus.Error.UnknownMethod"
                && reply.header().sender().map(|sender| sender.as_str()) == Some(owner))
    }

    fn error_source(error: &zbus::Error) -> Source {
        match error {
            zbus::Error::MethodError(name, _, _) => error_name_source(name.as_str()),
            zbus::Error::InputOutput(_) | zbus::Error::Connection(_, _) => Source::Disconnected,
            zbus::Error::Variant(_) | zbus::Error::InvalidReply => Source::InvalidType,
            _ => Source::Unavailable,
        }
    }

    fn error_name_source(name: &str) -> Source {
        match name {
            "org.freedesktop.DBus.Error.AccessDenied"
            | "org.freedesktop.DBus.Error.AuthFailed"
            | "org.freedesktop.portal.Error.NotAllowed" => Source::Denied,
            "org.freedesktop.portal.Error.NotFound" => Source::MissingKey,
            "org.freedesktop.DBus.Error.NameHasNoOwner"
            | "org.freedesktop.DBus.Error.ServiceUnknown" => Source::NoService,
            "org.freedesktop.DBus.Error.Disconnected" => Source::Disconnected,
            "org.freedesktop.DBus.Error.NoReply" | "org.freedesktop.DBus.Error.Timeout" => {
                Source::Timeout
            }
            _ => Source::Unavailable,
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use jterm_core::organism_motion::SystemMotion;

        fn signal(sender: &str, namespace: &str, key: &str, value: Value<'_>) -> Message {
            Message::signal(PATH, SETTINGS, "SettingChanged")
                .unwrap()
                .sender(sender)
                .unwrap()
                .build(&(namespace, key, value))
                .unwrap()
        }

        #[test]
        fn only_exact_u32_payloads_are_positive_observations() {
            for (value, expected) in [
                (0, SystemMotion::NoPreference),
                (1, SystemMotion::Reduce),
                (2, SystemMotion::NoPreference),
                (u32::MAX, SystemMotion::NoPreference),
            ] {
                assert_eq!(decode_value(&Value::U32(value), false).observed, expected);
                assert_eq!(
                    decode_value(&Value::Value(Box::new(Value::U32(value))), true).observed,
                    expected
                );
            }
            for value in [
                Value::Bool(false),
                Value::I32(0),
                Value::U64(0),
                Value::Str("0".into()),
            ] {
                assert_eq!(
                    decode_value(&value, false),
                    Fact::unavailable(Source::InvalidType)
                );
            }
            assert_eq!(
                decode_value(&Value::U32(0), true).source,
                Source::InvalidType
            );
            assert_eq!(
                decode_value(&Value::Value(Box::new(Value::U32(0))), false).source,
                Source::InvalidType
            );
        }

        #[test]
        fn read_reply_requires_the_expected_owner_and_variant_depth() {
            let call = Message::method_call(PATH, "ReadOne")
                .unwrap()
                .sender(":1.9")
                .unwrap()
                .build(&(NAMESPACE, KEY))
                .unwrap();
            let reply = |value| {
                Message::method_return(&call.header())
                    .unwrap()
                    .sender(":1.2")
                    .unwrap()
                    .build(&value)
                    .unwrap()
            };
            let direct = reply(Value::U32(0));
            assert_eq!(
                decode_reply(&direct, ":1.2", false).observed,
                SystemMotion::NoPreference
            );
            assert_eq!(
                decode_reply(&direct, ":1.3", false).observed,
                SystemMotion::Unavailable
            );
            assert_eq!(
                decode_reply(&direct, ":1.2", true).source,
                Source::InvalidType
            );
            let legacy = reply(Value::Value(Box::new(Value::U32(1))));
            assert_eq!(
                decode_reply(&legacy, ":1.2", true).observed,
                SystemMotion::Reduce
            );
            assert_eq!(
                decode_reply(&legacy, ":1.2", false).source,
                Source::InvalidType
            );
            let nested = reply(Value::Value(Box::new(Value::Value(Box::new(Value::U32(
                0,
            ))))));
            assert_eq!(
                decode_reply(&nested, ":1.2", true).source,
                Source::InvalidType
            );
            let wrong = Message::method_return(&call.header())
                .unwrap()
                .sender(":1.2")
                .unwrap()
                .build(&0_u32)
                .unwrap();
            assert_eq!(
                decode_reply(&wrong, ":1.2", false).source,
                Source::InvalidType
            );
        }

        #[test]
        fn signal_sender_namespace_key_and_variant_are_all_checked() {
            let good = signal(":1.2", NAMESPACE, KEY, Value::U32(1));
            assert_eq!(
                setting_change(&good, ":1.2").unwrap().observed,
                SystemMotion::Reduce
            );
            assert!(setting_change(&good, ":1.3").is_none());
            assert!(setting_change(&signal(":1.2", "other", KEY, Value::U32(0)), ":1.2").is_none());
            assert!(
                setting_change(&signal(":1.2", NAMESPACE, "other", Value::U32(0)), ":1.2")
                    .is_none()
            );
            assert_eq!(
                setting_change(&signal(":1.2", NAMESPACE, KEY, Value::Bool(false)), ":1.2")
                    .unwrap()
                    .source,
                Source::InvalidType
            );
            let non_variant = Message::signal(PATH, SETTINGS, "SettingChanged")
                .unwrap()
                .sender(":1.2")
                .unwrap()
                .build(&(NAMESPACE, KEY, 0_u32))
                .unwrap();
            assert_eq!(
                setting_change(&non_variant, ":1.2").unwrap().source,
                Source::InvalidType
            );
        }

        #[test]
        fn owner_messages_are_bus_authenticated_and_service_scoped() {
            let owner = |sender, name| {
                Message::signal(BUS_PATH, BUS, "NameOwnerChanged")
                    .unwrap()
                    .sender(sender)
                    .unwrap()
                    .build(&(name, ":1.2", ":1.3"))
                    .unwrap()
            };
            assert!(owner_change(&owner(":1.9", SERVICE)).is_none());
            assert!(owner_change(&owner(BUS, "other.service")).is_none());
            assert_eq!(
                owner_change(&owner(BUS, SERVICE))
                    .unwrap()
                    .unwrap()
                    .unwrap()
                    .as_str(),
                ":1.3"
            );
        }

        #[test]
        fn only_authenticated_unknown_method_allows_the_legacy_route() {
            let call = Message::method_call(PATH, "ReadOne")
                .unwrap()
                .sender(":1.9")
                .unwrap()
                .build(&(NAMESPACE, KEY))
                .unwrap();
            let error = |sender, name| {
                let reply = Message::error(&call.header(), name)
                    .unwrap()
                    .sender(sender)
                    .unwrap()
                    .build(&"ignored detail")
                    .unwrap();
                zbus::Error::from(reply)
            };
            let unknown = "org.freedesktop.DBus.Error.UnknownMethod";
            let denied = "org.freedesktop.DBus.Error.AccessDenied";
            assert!(unknown_method_from(&error(":1.2", unknown), ":1.2"));
            assert!(!unknown_method_from(&error(":1.3", unknown), ":1.2"));
            assert!(!unknown_method_from(&error(":1.2", denied), ":1.2"));
            assert_eq!(error_source(&error(":1.2", denied)), Source::Denied);
            assert_eq!(
                error_name_source("org.freedesktop.portal.Error.NotFound"),
                Source::MissingKey
            );
        }

        #[test]
        fn replacement_invalidates_read_and_old_signal_sender() {
            let mut provider = Provider::new();
            provider
                .replace(Some(OwnedUniqueName::try_from(":1.2").unwrap()))
                .unwrap();
            let old = provider.epoch.unwrap();
            let read = provider.observation.begin_read(old).unwrap();
            provider
                .replace(Some(OwnedUniqueName::try_from(":1.3").unwrap()))
                .unwrap();
            assert!(!provider
                .observation
                .complete_read(read, SystemMotion::NoPreference));
            assert!(!provider
                .observation
                .signal(old, 1, SystemMotion::NoPreference));
            assert_eq!(provider.observation.value(), SystemMotion::Unavailable);
            provider.replace(None).unwrap();
            assert_eq!(provider.observation.value(), SystemMotion::Unavailable);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delivery_is_deduplicated_but_retains_reason_changes() {
        let mut emitter = Emitter::new(7);
        let initializing = Fact::unavailable(Source::Initializing);
        assert_eq!(emitter.event(initializing).unwrap().sequence, 1);
        assert!(emitter.event(initializing).is_none());
        let denied = emitter.event(Fact::unavailable(Source::Denied)).unwrap();
        assert_eq!(denied.subscription_epoch, 7);
        assert_eq!(denied.sequence, 2);
        assert_eq!(denied.observed, SystemMotion::Unavailable);
        let observed = Fact {
            observed: SystemMotion::NoPreference,
            source: Source::Portal,
        };
        assert_eq!(emitter.event(observed).unwrap().sequence, 3);
        assert!(emitter.event(observed).is_none());
    }

    #[test]
    fn delivery_sequence_exhaustion_clears_full_without_reusing_identity() {
        let mut emitter = Emitter::new(9);
        emitter.sequence = u64::MAX - 1;
        let event = emitter
            .event(Fact {
                observed: SystemMotion::NoPreference,
                source: Source::Portal,
            })
            .unwrap();
        assert_eq!(event.sequence, u64::MAX);
        assert_eq!(event.observed, SystemMotion::Unavailable);
        assert_eq!(event.source, Source::IdentityExhausted);
        assert!(emitter.event(Fact::unavailable(Source::Denied)).is_none());
    }
}
