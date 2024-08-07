mod data;

use std::{
	collections::{BTreeMap, HashSet},
	fmt::Write,
	sync::Arc,
};

use conduit::{
	debug, error, info, utils,
	utils::{MutexMap, MutexMapGuard},
	validated, warn, Error, Result,
};
use data::Data;
use itertools::Itertools;
use ruma::{
	api::{client::error::ErrorKind, federation},
	canonical_json::to_canonical_value,
	events::{
		push_rules::PushRulesEvent,
		room::{
			create::RoomCreateEventContent,
			encrypted::Relation,
			member::{MembershipState, RoomMemberEventContent},
			power_levels::RoomPowerLevelsEventContent,
			redaction::RoomRedactionEventContent,
		},
		GlobalAccountDataEventType, StateEventType, TimelineEventType,
	},
	push::{Action, Ruleset, Tweak},
	serde::Base64,
	state_res::{self, Event, RoomVersion},
	uint, user_id, CanonicalJsonObject, CanonicalJsonValue, EventId, OwnedEventId, OwnedRoomId, OwnedServerName,
	RoomId, RoomVersionId, ServerName, UserId,
};
use serde::Deserialize;
use serde_json::value::{to_raw_value, RawValue as RawJsonValue};
use tokio::sync::RwLock;

use crate::{
	admin,
	appservice::NamespaceRegex,
	pdu::{EventHash, PduBuilder},
	rooms::{event_handler::parse_incoming_pdu, state_compressor::CompressedStateEvent},
	server_is_ours, services, PduCount, PduEvent,
};

// Update Relationships
#[derive(Deserialize)]
struct ExtractRelatesTo {
	#[serde(rename = "m.relates_to")]
	relates_to: Relation,
}

#[derive(Clone, Debug, Deserialize)]
struct ExtractEventId {
	event_id: OwnedEventId,
}
#[derive(Clone, Debug, Deserialize)]
struct ExtractRelatesToEventId {
	#[serde(rename = "m.relates_to")]
	relates_to: ExtractEventId,
}

#[derive(Deserialize)]
struct ExtractBody {
	body: Option<String>,
}

pub struct Service {
	db: Data,
	pub mutex_insert: RoomMutexMap,
}

type RoomMutexMap = MutexMap<OwnedRoomId, ()>;
pub type RoomMutexGuard = MutexMapGuard<OwnedRoomId, ()>;

impl crate::Service for Service {
	fn build(args: crate::Args<'_>) -> Result<Arc<Self>> {
		Ok(Arc::new(Self {
			db: Data::new(args.db),
			mutex_insert: RoomMutexMap::new(),
		}))
	}

	fn memory_usage(&self, out: &mut dyn Write) -> Result<()> {
		let lasttimelinecount_cache = self
			.db
			.lasttimelinecount_cache
			.lock()
			.expect("locked")
			.len();
		writeln!(out, "lasttimelinecount_cache: {lasttimelinecount_cache}")?;

		let mutex_insert = self.mutex_insert.len();
		writeln!(out, "insert_mutex: {mutex_insert}")?;

		Ok(())
	}

	fn clear_cache(&self) {
		self.db
			.lasttimelinecount_cache
			.lock()
			.expect("locked")
			.clear();
	}

	fn name(&self) -> &str { crate::service::make_name(std::module_path!()) }
}

impl Service {
	#[tracing::instrument(skip(self), level = "debug")]
	pub fn first_pdu_in_room(&self, room_id: &RoomId) -> Result<Option<Arc<PduEvent>>> {
		self.all_pdus(user_id!("@doesntmatter:conduit.rs"), room_id)?
			.next()
			.map(|o| o.map(|(_, p)| Arc::new(p)))
			.transpose()
	}

	#[tracing::instrument(skip(self), level = "debug")]
	pub fn latest_pdu_in_room(&self, room_id: &RoomId) -> Result<Option<Arc<PduEvent>>> {
		self.all_pdus(user_id!("@placeholder:conduwuit.placeholder"), room_id)?
			.last()
			.map(|o| o.map(|(_, p)| Arc::new(p)))
			.transpose()
	}

	#[tracing::instrument(skip(self), level = "debug")]
	pub fn last_timeline_count(&self, sender_user: &UserId, room_id: &RoomId) -> Result<PduCount> {
		self.db.last_timeline_count(sender_user, room_id)
	}

	/// Returns the `count` of this pdu's id.
	pub fn get_pdu_count(&self, event_id: &EventId) -> Result<Option<PduCount>> { self.db.get_pdu_count(event_id) }

	// TODO Is this the same as the function above?
	/*
	#[tracing::instrument(skip(self))]
	pub fn latest_pdu_count(&self, room_id: &RoomId) -> Result<u64> {
		let prefix = self
			.get_shortroomid(room_id)?
			.expect("room exists")
			.to_be_bytes()
			.to_vec();

		let mut last_possible_key = prefix.clone();
		last_possible_key.extend_from_slice(&u64::MAX.to_be_bytes());

		self.pduid_pdu
			.iter_from(&last_possible_key, true)
			.take_while(move |(k, _)| k.starts_with(&prefix))
			.next()
			.map(|b| self.pdu_count(&b.0))
			.transpose()
			.map(|op| op.unwrap_or_default())
	}
	*/

	/// Returns the json of a pdu.
	pub fn get_pdu_json(&self, event_id: &EventId) -> Result<Option<CanonicalJsonObject>> {
		self.db.get_pdu_json(event_id)
	}

	/// Returns the json of a pdu.
	#[inline]
	pub fn get_non_outlier_pdu_json(&self, event_id: &EventId) -> Result<Option<CanonicalJsonObject>> {
		self.db.get_non_outlier_pdu_json(event_id)
	}

	/// Returns the pdu's id.
	#[inline]
	pub fn get_pdu_id(&self, event_id: &EventId) -> Result<Option<database::Handle<'_>>> {
		self.db.get_pdu_id(event_id)
	}

	/// Returns the pdu.
	///
	/// Checks the `eventid_outlierpdu` Tree if not found in the timeline.
	#[inline]
	pub fn get_non_outlier_pdu(&self, event_id: &EventId) -> Result<Option<PduEvent>> {
		self.db.get_non_outlier_pdu(event_id)
	}

	/// Returns the pdu.
	///
	/// Checks the `eventid_outlierpdu` Tree if not found in the timeline.
	pub fn get_pdu(&self, event_id: &EventId) -> Result<Option<Arc<PduEvent>>> { self.db.get_pdu(event_id) }

	/// Returns the pdu.
	///
	/// This does __NOT__ check the outliers `Tree`.
	pub fn get_pdu_from_id(&self, pdu_id: &[u8]) -> Result<Option<PduEvent>> { self.db.get_pdu_from_id(pdu_id) }

	/// Returns the pdu as a `BTreeMap<String, CanonicalJsonValue>`.
	pub fn get_pdu_json_from_id(&self, pdu_id: &[u8]) -> Result<Option<CanonicalJsonObject>> {
		self.db.get_pdu_json_from_id(pdu_id)
	}

	/// Removes a pdu and creates a new one with the same id.
	#[tracing::instrument(skip(self), level = "debug")]
	pub fn replace_pdu(&self, pdu_id: &[u8], pdu_json: &CanonicalJsonObject, pdu: &PduEvent) -> Result<()> {
		self.db.replace_pdu(pdu_id, pdu_json, pdu)
	}

	/// Creates a new persisted data unit and adds it to a room.
	///
	/// By this point the incoming event should be fully authenticated, no auth
	/// happens in `append_pdu`.
	///
	/// Returns pdu id
	#[tracing::instrument(skip_all)]
	pub async fn append_pdu(
		&self,
		pdu: &PduEvent,
		mut pdu_json: CanonicalJsonObject,
		leaves: Vec<OwnedEventId>,
		state_lock: &RoomMutexGuard, // Take mutex guard to make sure users get the room state mutex
	) -> Result<Vec<u8>> {
		// Coalesce database writes for the remainder of this scope.
		let _cork = services().db.cork_and_flush();

		let shortroomid = services()
			.rooms
			.short
			.get_shortroomid(&pdu.room_id)?
			.expect("room exists");

		// Make unsigned fields correct. This is not properly documented in the spec,
		// but state events need to have previous content in the unsigned field, so
		// clients can easily interpret things like membership changes
		if let Some(state_key) = &pdu.state_key {
			if let CanonicalJsonValue::Object(unsigned) = pdu_json
				.entry("unsigned".to_owned())
				.or_insert_with(|| CanonicalJsonValue::Object(BTreeMap::default()))
			{
				if let Some(shortstatehash) = services()
					.rooms
					.state_accessor
					.pdu_shortstatehash(&pdu.event_id)
					.unwrap()
				{
					if let Some(prev_state) = services()
						.rooms
						.state_accessor
						.state_get(shortstatehash, &pdu.kind.to_string().into(), state_key)
						.unwrap()
					{
						unsigned.insert(
							"prev_content".to_owned(),
							CanonicalJsonValue::Object(
								utils::to_canonical_object(prev_state.content.clone()).map_err(|e| {
									error!("Failed to convert prev_state to canonical JSON: {e}");
									Error::bad_database("Failed to convert prev_state to canonical JSON.")
								})?,
							),
						);
						unsigned.insert(
							String::from("prev_sender"),
							CanonicalJsonValue::String(prev_state.sender.clone().to_string()),
						);
						unsigned.insert(
							String::from("replaces_state"),
							CanonicalJsonValue::String(prev_state.event_id.clone().to_string()),
						);
					}
				}
			} else {
				error!("Invalid unsigned type in pdu.");
			}
		}

		// We must keep track of all events that have been referenced.
		services()
			.rooms
			.pdu_metadata
			.mark_as_referenced(&pdu.room_id, &pdu.prev_events)?;
		services()
			.rooms
			.state
			.set_forward_extremities(&pdu.room_id, leaves, state_lock)?;

		let insert_lock = self.mutex_insert.lock(&pdu.room_id).await;

		let count1 = services().globals.next_count()?;
		// Mark as read first so the sending client doesn't get a notification even if
		// appending fails
		services()
			.rooms
			.read_receipt
			.private_read_set(&pdu.room_id, &pdu.sender, count1)?;
		services()
			.rooms
			.user
			.reset_notification_counts(&pdu.sender, &pdu.room_id)?;

		let count2 = services().globals.next_count()?;
		let mut pdu_id = shortroomid.to_be_bytes().to_vec();
		pdu_id.extend_from_slice(&count2.to_be_bytes());

		// Insert pdu
		self.db.append_pdu(&pdu_id, pdu, &pdu_json, count2)?;

		drop(insert_lock);

		// See if the event matches any known pushers
		let power_levels: RoomPowerLevelsEventContent = services()
			.rooms
			.state_accessor
			.room_state_get(&pdu.room_id, &StateEventType::RoomPowerLevels, "")?
			.map(|ev| {
				serde_json::from_str(ev.content.get())
					.map_err(|_| Error::bad_database("invalid m.room.power_levels event"))
			})
			.transpose()?
			.unwrap_or_default();

		let sync_pdu = pdu.to_sync_room_event();

		let mut notifies = Vec::new();
		let mut highlights = Vec::new();

		let mut push_target = services()
			.rooms
			.state_cache
			.active_local_users_in_room(&pdu.room_id)
			.collect_vec();

		if pdu.kind == TimelineEventType::RoomMember {
			if let Some(state_key) = &pdu.state_key {
				let target_user_id = UserId::parse(state_key.clone()).expect("This state_key was previously validated");

				if !push_target.contains(&target_user_id) {
					push_target.push(target_user_id);
				}
			}
		}

		for user in &push_target {
			// Don't notify the user of their own events
			if user == &pdu.sender {
				continue;
			}

			let rules_for_user = services()
				.account_data
				.get(None, user, GlobalAccountDataEventType::PushRules.to_string().into())?
				.map(|event| {
					serde_json::from_str::<PushRulesEvent>(event.get()).map_err(|e| {
						warn!("Invalid push rules event in db for user ID {user}: {e}");
						Error::bad_database("Invalid push rules event in db.")
					})
				})
				.transpose()?
				.map_or_else(|| Ruleset::server_default(user), |ev: PushRulesEvent| ev.content.global);

			let mut highlight = false;
			let mut notify = false;

			for action in
				services()
					.pusher
					.get_actions(user, &rules_for_user, &power_levels, &sync_pdu, &pdu.room_id)?
			{
				match action {
					Action::Notify => notify = true,
					Action::SetTweak(Tweak::Highlight(true)) => {
						highlight = true;
					},
					_ => {},
				};
			}

			if notify {
				notifies.push(user.clone());
			}

			if highlight {
				highlights.push(user.clone());
			}

			for push_key in services().pusher.get_pushkeys(user) {
				services().sending.send_pdu_push(&pdu_id, user, push_key?)?;
			}
		}

		self.db
			.increment_notification_counts(&pdu.room_id, notifies, highlights)?;

		match pdu.kind {
			TimelineEventType::RoomRedaction => {
				use RoomVersionId::*;

				let room_version_id = services().rooms.state.get_room_version(&pdu.room_id)?;
				match room_version_id {
					V1 | V2 | V3 | V4 | V5 | V6 | V7 | V8 | V9 | V10 => {
						if let Some(redact_id) = &pdu.redacts {
							if services().rooms.state_accessor.user_can_redact(
								redact_id,
								&pdu.sender,
								&pdu.room_id,
								false,
							)? {
								self.redact_pdu(redact_id, pdu, shortroomid)?;
							}
						}
					},
					V11 => {
						let content =
							serde_json::from_str::<RoomRedactionEventContent>(pdu.content.get()).map_err(|e| {
								warn!("Invalid content in redaction pdu: {e}");
								Error::bad_database("Invalid content in redaction pdu")
							})?;

						if let Some(redact_id) = &content.redacts {
							if services().rooms.state_accessor.user_can_redact(
								redact_id,
								&pdu.sender,
								&pdu.room_id,
								false,
							)? {
								self.redact_pdu(redact_id, pdu, shortroomid)?;
							}
						}
					},
					_ => {
						warn!("Unexpected or unsupported room version {room_version_id}");
						return Err(Error::BadRequest(
							ErrorKind::BadJson,
							"Unexpected or unsupported room version found",
						));
					},
				};
			},
			TimelineEventType::SpaceChild => {
				if let Some(_state_key) = &pdu.state_key {
					services()
						.rooms
						.spaces
						.roomid_spacehierarchy_cache
						.lock()
						.await
						.remove(&pdu.room_id);
				}
			},
			TimelineEventType::RoomMember => {
				if let Some(state_key) = &pdu.state_key {
					// if the state_key fails
					let target_user_id =
						UserId::parse(state_key.clone()).expect("This state_key was previously validated");

					let content = serde_json::from_str::<RoomMemberEventContent>(pdu.content.get()).map_err(|e| {
						error!("Invalid room member event content in pdu: {e}");
						Error::bad_database("Invalid room member event content in pdu.")
					})?;

					let invite_state = match content.membership {
						MembershipState::Invite => {
							let state = services().rooms.state.calculate_invite_state(pdu)?;
							Some(state)
						},
						_ => None,
					};

					// Update our membership info, we do this here incase a user is invited
					// and immediately leaves we need the DB to record the invite event for auth
					services().rooms.state_cache.update_membership(
						&pdu.room_id,
						&target_user_id,
						content,
						&pdu.sender,
						invite_state,
						None,
						true,
					)?;
				}
			},
			TimelineEventType::RoomMessage => {
				let content = serde_json::from_str::<ExtractBody>(pdu.content.get())
					.map_err(|_| Error::bad_database("Invalid content in pdu."))?;

				if let Some(body) = content.body {
					services()
						.rooms
						.search
						.index_pdu(shortroomid, &pdu_id, &body)?;

					if admin::is_admin_command(pdu, &body).await {
						services()
							.admin
							.command(body, Some((*pdu.event_id).into()))
							.await;
					}
				}
			},
			_ => {},
		}

		if let Ok(content) = serde_json::from_str::<ExtractRelatesToEventId>(pdu.content.get()) {
			if let Some(related_pducount) = self.get_pdu_count(&content.relates_to.event_id)? {
				services()
					.rooms
					.pdu_metadata
					.add_relation(PduCount::Normal(count2), related_pducount)?;
			}
		}

		if let Ok(content) = serde_json::from_str::<ExtractRelatesTo>(pdu.content.get()) {
			match content.relates_to {
				Relation::Reply {
					in_reply_to,
				} => {
					// We need to do it again here, because replies don't have
					// event_id as a top level field
					if let Some(related_pducount) = self.get_pdu_count(&in_reply_to.event_id)? {
						services()
							.rooms
							.pdu_metadata
							.add_relation(PduCount::Normal(count2), related_pducount)?;
					}
				},
				Relation::Thread(thread) => {
					services()
						.rooms
						.threads
						.add_to_thread(&thread.event_id, pdu)?;
				},
				_ => {}, // TODO: Aggregate other types
			}
		}

		for appservice in services().appservice.read().await.values() {
			if services()
				.rooms
				.state_cache
				.appservice_in_room(&pdu.room_id, appservice)?
			{
				services()
					.sending
					.send_pdu_appservice(appservice.registration.id.clone(), pdu_id.clone())?;
				continue;
			}

			// If the RoomMember event has a non-empty state_key, it is targeted at someone.
			// If it is our appservice user, we send this PDU to it.
			if pdu.kind == TimelineEventType::RoomMember {
				if let Some(state_key_uid) = &pdu
					.state_key
					.as_ref()
					.and_then(|state_key| UserId::parse(state_key.as_str()).ok())
				{
					let appservice_uid = appservice.registration.sender_localpart.as_str();
					if state_key_uid == appservice_uid {
						services()
							.sending
							.send_pdu_appservice(appservice.registration.id.clone(), pdu_id.clone())?;
						continue;
					}
				}
			}

			let matching_users = |users: &NamespaceRegex| {
				appservice.users.is_match(pdu.sender.as_str())
					|| pdu.kind == TimelineEventType::RoomMember
						&& pdu
							.state_key
							.as_ref()
							.map_or(false, |state_key| users.is_match(state_key))
			};
			let matching_aliases = |aliases: &NamespaceRegex| {
				services()
					.rooms
					.alias
					.local_aliases_for_room(&pdu.room_id)
					.filter_map(Result::ok)
					.any(|room_alias| aliases.is_match(room_alias.as_str()))
			};

			if matching_aliases(&appservice.aliases)
				|| appservice.rooms.is_match(pdu.room_id.as_str())
				|| matching_users(&appservice.users)
			{
				services()
					.sending
					.send_pdu_appservice(appservice.registration.id.clone(), pdu_id.clone())?;
			}
		}

		Ok(pdu_id)
	}

	pub fn create_hash_and_sign_event(
		&self,
		pdu_builder: PduBuilder,
		sender: &UserId,
		room_id: &RoomId,
		_mutex_lock: &RoomMutexGuard, // Take mutex guard to make sure users get the room state mutex
	) -> Result<(PduEvent, CanonicalJsonObject)> {
		let PduBuilder {
			event_type,
			content,
			unsigned,
			state_key,
			redacts,
		} = pdu_builder;

		let prev_events: Vec<_> = services()
			.rooms
			.state
			.get_forward_extremities(room_id)?
			.into_iter()
			.take(20)
			.collect();

		// If there was no create event yet, assume we are creating a room
		let room_version_id = services()
			.rooms
			.state
			.get_room_version(room_id)
			.or_else(|_| {
				if event_type == TimelineEventType::RoomCreate {
					let content = serde_json::from_str::<RoomCreateEventContent>(content.get())
						.expect("Invalid content in RoomCreate pdu.");
					Ok(content.room_version)
				} else {
					Err(Error::InconsistentRoomState(
						"non-create event for room of unknown version",
						room_id.to_owned(),
					))
				}
			})?;

		let room_version = RoomVersion::new(&room_version_id).expect("room version is supported");

		let auth_events =
			services()
				.rooms
				.state
				.get_auth_events(room_id, &event_type, sender, state_key.as_deref(), &content)?;

		// Our depth is the maximum depth of prev_events + 1
		let depth = prev_events
			.iter()
			.filter_map(|event_id| Some(self.get_pdu(event_id).ok()??.depth))
			.max()
			.unwrap_or_else(|| uint!(0))
			.saturating_add(uint!(1));

		let mut unsigned = unsigned.unwrap_or_default();

		if let Some(state_key) = &state_key {
			if let Some(prev_pdu) =
				services()
					.rooms
					.state_accessor
					.room_state_get(room_id, &event_type.to_string().into(), state_key)?
			{
				unsigned.insert(
					"prev_content".to_owned(),
					serde_json::from_str(prev_pdu.content.get()).expect("string is valid json"),
				);
				unsigned.insert(
					"prev_sender".to_owned(),
					serde_json::to_value(&prev_pdu.sender).expect("UserId::to_value always works"),
				);
				unsigned.insert(
					"replaces_state".to_owned(),
					serde_json::to_value(&prev_pdu.event_id).expect("EventId is valid json"),
				);
			}
		}

		let mut pdu = PduEvent {
			event_id: ruma::event_id!("$thiswillbefilledinlater").into(),
			room_id: room_id.to_owned(),
			sender: sender.to_owned(),
			origin: None,
			origin_server_ts: utils::millis_since_unix_epoch()
				.try_into()
				.expect("time is valid"),
			kind: event_type,
			content,
			state_key,
			prev_events,
			depth,
			auth_events: auth_events
				.values()
				.map(|pdu| pdu.event_id.clone())
				.collect(),
			redacts,
			unsigned: if unsigned.is_empty() {
				None
			} else {
				Some(to_raw_value(&unsigned).expect("to_raw_value always works"))
			},
			hashes: EventHash {
				sha256: "aaa".to_owned(),
			},
			signatures: None,
		};

		let auth_check = state_res::auth_check(
			&room_version,
			&pdu,
			None::<PduEvent>, // TODO: third_party_invite
			|k, s| auth_events.get(&(k.clone(), s.to_owned())),
		)
		.map_err(|e| {
			error!("Auth check failed: {:?}", e);
			Error::BadRequest(ErrorKind::forbidden(), "Auth check failed.")
		})?;

		if !auth_check {
			return Err(Error::BadRequest(ErrorKind::forbidden(), "Event is not authorized."));
		}

		// Hash and sign
		let mut pdu_json = utils::to_canonical_object(&pdu).map_err(|e| {
			error!("Failed to convert PDU to canonical JSON: {e}");
			Error::bad_database("Failed to convert PDU to canonical JSON.")
		})?;

		// room v3 and above removed the "event_id" field from remote PDU format
		match room_version_id {
			RoomVersionId::V1 | RoomVersionId::V2 => {},
			_ => {
				pdu_json.remove("event_id");
			},
		};

		// Add origin because synapse likes that (and it's required in the spec)
		pdu_json.insert(
			"origin".to_owned(),
			to_canonical_value(services().globals.server_name()).expect("server name is a valid CanonicalJsonValue"),
		);

		match ruma::signatures::hash_and_sign_event(
			services().globals.server_name().as_str(),
			services().globals.keypair(),
			&mut pdu_json,
			&room_version_id,
		) {
			Ok(()) => {},
			Err(e) => {
				return match e {
					ruma::signatures::Error::PduSize => {
						Err(Error::BadRequest(ErrorKind::TooLarge, "Message is too long"))
					},
					_ => Err(Error::BadRequest(ErrorKind::Unknown, "Signing event failed")),
				}
			},
		}

		// Generate event id
		pdu.event_id = EventId::parse_arc(format!(
			"${}",
			ruma::signatures::reference_hash(&pdu_json, &room_version_id).expect("ruma can calculate reference hashes")
		))
		.expect("ruma's reference hashes are valid event ids");

		pdu_json.insert(
			"event_id".to_owned(),
			CanonicalJsonValue::String(pdu.event_id.as_str().to_owned()),
		);

		// Generate short event id
		let _shorteventid = services()
			.rooms
			.short
			.get_or_create_shorteventid(&pdu.event_id)?;

		Ok((pdu, pdu_json))
	}

	/// Creates a new persisted data unit and adds it to a room. This function
	/// takes a roomid_mutex_state, meaning that only this function is able to
	/// mutate the room state.
	#[tracing::instrument(skip(self, state_lock))]
	pub async fn build_and_append_pdu(
		&self,
		pdu_builder: PduBuilder,
		sender: &UserId,
		room_id: &RoomId,
		state_lock: &RoomMutexGuard, // Take mutex guard to make sure users get the room state mutex
	) -> Result<Arc<EventId>> {
		let (pdu, pdu_json) = self.create_hash_and_sign_event(pdu_builder, sender, room_id, state_lock)?;
		if let Some(admin_room) = admin::Service::get_admin_room()? {
			if admin_room == room_id {
				match pdu.event_type() {
					TimelineEventType::RoomEncryption => {
						warn!("Encryption is not allowed in the admins room");
						return Err(Error::BadRequest(
							ErrorKind::forbidden(),
							"Encryption is not allowed in the admins room",
						));
					},
					TimelineEventType::RoomMember => {
						let target = pdu
							.state_key()
							.filter(|v| v.starts_with('@'))
							.unwrap_or(sender.as_str());
						let server_user = &services().globals.server_user.to_string();

						let content = serde_json::from_str::<RoomMemberEventContent>(pdu.content.get())
							.map_err(|_| Error::bad_database("Invalid content in pdu"))?;

						if content.membership == MembershipState::Leave {
							if target == server_user {
								warn!("Server user cannot leave from admins room");
								return Err(Error::BadRequest(
									ErrorKind::forbidden(),
									"Server user cannot leave from admins room.",
								));
							}

							let count = services()
								.rooms
								.state_cache
								.room_members(room_id)
								.filter_map(Result::ok)
								.filter(|m| server_is_ours(m.server_name()) && m != target)
								.count();
							if count < 2 {
								warn!("Last admin cannot leave from admins room");
								return Err(Error::BadRequest(
									ErrorKind::forbidden(),
									"Last admin cannot leave from admins room.",
								));
							}
						}

						if content.membership == MembershipState::Ban && pdu.state_key().is_some() {
							if target == server_user {
								warn!("Server user cannot be banned in admins room");
								return Err(Error::BadRequest(
									ErrorKind::forbidden(),
									"Server user cannot be banned in admins room.",
								));
							}

							let count = services()
								.rooms
								.state_cache
								.room_members(room_id)
								.filter_map(Result::ok)
								.filter(|m| server_is_ours(m.server_name()) && m != target)
								.count();
							if count < 2 {
								warn!("Last admin cannot be banned in admins room");
								return Err(Error::BadRequest(
									ErrorKind::forbidden(),
									"Last admin cannot be banned in admins room.",
								));
							}
						}
					},
					_ => {},
				}
			}
		}

		// If redaction event is not authorized, do not append it to the timeline
		if pdu.kind == TimelineEventType::RoomRedaction {
			use RoomVersionId::*;
			match services().rooms.state.get_room_version(&pdu.room_id)? {
				V1 | V2 | V3 | V4 | V5 | V6 | V7 | V8 | V9 | V10 => {
					if let Some(redact_id) = &pdu.redacts {
						if !services().rooms.state_accessor.user_can_redact(
							redact_id,
							&pdu.sender,
							&pdu.room_id,
							false,
						)? {
							return Err(Error::BadRequest(ErrorKind::forbidden(), "User cannot redact this event."));
						}
					};
				},
				_ => {
					let content = serde_json::from_str::<RoomRedactionEventContent>(pdu.content.get())
						.map_err(|_| Error::bad_database("Invalid content in redaction pdu."))?;

					if let Some(redact_id) = &content.redacts {
						if !services().rooms.state_accessor.user_can_redact(
							redact_id,
							&pdu.sender,
							&pdu.room_id,
							false,
						)? {
							return Err(Error::BadRequest(ErrorKind::forbidden(), "User cannot redact this event."));
						}
					}
				},
			}
		};

		// We append to state before appending the pdu, so we don't have a moment in
		// time with the pdu without it's state. This is okay because append_pdu can't
		// fail.
		let statehashid = services().rooms.state.append_to_state(&pdu)?;

		let pdu_id = self
			.append_pdu(
				&pdu,
				pdu_json,
				// Since this PDU references all pdu_leaves we can update the leaves
				// of the room
				vec![(*pdu.event_id).to_owned()],
				state_lock,
			)
			.await?;

		// We set the room state after inserting the pdu, so that we never have a moment
		// in time where events in the current room state do not exist
		services()
			.rooms
			.state
			.set_room_state(room_id, statehashid, state_lock)?;

		let mut servers: HashSet<OwnedServerName> = services()
			.rooms
			.state_cache
			.room_servers(room_id)
			.filter_map(Result::ok)
			.collect();

		// In case we are kicking or banning a user, we need to inform their server of
		// the change
		if pdu.kind == TimelineEventType::RoomMember {
			if let Some(state_key_uid) = &pdu
				.state_key
				.as_ref()
				.and_then(|state_key| UserId::parse(state_key.as_str()).ok())
			{
				servers.insert(state_key_uid.server_name().to_owned());
			}
		}

		// Remove our server from the server list since it will be added to it by
		// room_servers() and/or the if statement above
		servers.remove(services().globals.server_name());

		services()
			.sending
			.send_pdu_servers(servers.into_iter(), &pdu_id)?;

		Ok(pdu.event_id)
	}

	/// Append the incoming event setting the state snapshot to the state from
	/// the server that sent the event.
	#[tracing::instrument(skip_all)]
	pub async fn append_incoming_pdu(
		&self,
		pdu: &PduEvent,
		pdu_json: CanonicalJsonObject,
		new_room_leaves: Vec<OwnedEventId>,
		state_ids_compressed: Arc<HashSet<CompressedStateEvent>>,
		soft_fail: bool,
		state_lock: &RoomMutexGuard, // Take mutex guard to make sure users get the room state mutex
	) -> Result<Option<Vec<u8>>> {
		// We append to state before appending the pdu, so we don't have a moment in
		// time with the pdu without it's state. This is okay because append_pdu can't
		// fail.
		services()
			.rooms
			.state
			.set_event_state(&pdu.event_id, &pdu.room_id, state_ids_compressed)?;

		if soft_fail {
			services()
				.rooms
				.pdu_metadata
				.mark_as_referenced(&pdu.room_id, &pdu.prev_events)?;
			services()
				.rooms
				.state
				.set_forward_extremities(&pdu.room_id, new_room_leaves, state_lock)?;
			return Ok(None);
		}

		let pdu_id = self
			.append_pdu(pdu, pdu_json, new_room_leaves, state_lock)
			.await?;

		Ok(Some(pdu_id))
	}

	/// Returns an iterator over all PDUs in a room.
	#[inline]
	pub fn all_pdus<'a>(
		&'a self, user_id: &UserId, room_id: &RoomId,
	) -> Result<impl Iterator<Item = Result<(PduCount, PduEvent)>> + 'a> {
		self.pdus_after(user_id, room_id, PduCount::min())
	}

	/// Returns an iterator over all events and their tokens in a room that
	/// happened before the event with id `until` in reverse-chronological
	/// order.
	#[tracing::instrument(skip(self), level = "debug")]
	pub fn pdus_until<'a>(
		&'a self, user_id: &UserId, room_id: &RoomId, until: PduCount,
	) -> Result<impl Iterator<Item = Result<(PduCount, PduEvent)>> + 'a> {
		self.db.pdus_until(user_id, room_id, until)
	}

	/// Returns an iterator over all events and their token in a room that
	/// happened after the event with id `from` in chronological order.
	#[tracing::instrument(skip(self), level = "debug")]
	pub fn pdus_after<'a>(
		&'a self, user_id: &UserId, room_id: &RoomId, from: PduCount,
	) -> Result<impl Iterator<Item = Result<(PduCount, PduEvent)>> + 'a> {
		self.db.pdus_after(user_id, room_id, from)
	}

	/// Replace a PDU with the redacted form.
	#[tracing::instrument(skip(self, reason))]
	pub fn redact_pdu(&self, event_id: &EventId, reason: &PduEvent, shortroomid: u64) -> Result<()> {
		// TODO: Don't reserialize, keep original json
		if let Some(pdu_id) = self.get_pdu_id(event_id)? {
			let mut pdu = self
				.get_pdu_from_id(&pdu_id)?
				.ok_or_else(|| Error::bad_database("PDU ID points to invalid PDU."))?;

			if let Ok(content) = serde_json::from_str::<ExtractBody>(pdu.content.get()) {
				if let Some(body) = content.body {
					services()
						.rooms
						.search
						.deindex_pdu(shortroomid, &pdu_id, &body)?;
				}
			}

			let room_version_id = services().rooms.state.get_room_version(&pdu.room_id)?;

			pdu.redact(room_version_id, reason)?;

			self.replace_pdu(
				&pdu_id,
				&utils::to_canonical_object(&pdu).map_err(|e| {
					error!("Failed to convert PDU to canonical JSON: {}", e);
					Error::bad_database("Failed to convert PDU to canonical JSON.")
				})?,
				&pdu,
			)?;
		}
		// If event does not exist, just noop
		Ok(())
	}

	#[tracing::instrument(skip(self))]
	pub async fn backfill_if_required(&self, room_id: &RoomId, from: PduCount) -> Result<()> {
		let first_pdu = self
			.all_pdus(user_id!("@doesntmatter:conduit.rs"), room_id)?
			.next()
			.expect("Room is not empty")?;

		if first_pdu.0 < from {
			// No backfill required, there are still events between them
			return Ok(());
		}

		let power_levels: RoomPowerLevelsEventContent = services()
			.rooms
			.state_accessor
			.room_state_get(room_id, &StateEventType::RoomPowerLevels, "")?
			.map(|ev| {
				serde_json::from_str(ev.content.get())
					.map_err(|_| Error::bad_database("invalid m.room.power_levels event"))
			})
			.transpose()?
			.unwrap_or_default();

		let room_mods = power_levels.users.iter().filter_map(|(user_id, level)| {
			if level > &power_levels.users_default && !server_is_ours(user_id.server_name()) {
				Some(user_id.server_name().to_owned())
			} else {
				None
			}
		});

		let room_alias_servers = services()
			.rooms
			.alias
			.local_aliases_for_room(room_id)
			.filter_map(|alias| {
				alias
					.ok()
					.filter(|alias| !server_is_ours(alias.server_name()))
					.map(|alias| alias.server_name().to_owned())
			});

		let servers = room_mods
			.chain(room_alias_servers)
			.chain(services().globals.config.trusted_servers.clone())
			.filter(|server_name| {
				if server_is_ours(server_name) {
					return false;
				}

				services()
					.rooms
					.state_cache
					.server_in_room(server_name, room_id)
					.unwrap_or(false)
			});

		for backfill_server in servers {
			info!("Asking {backfill_server} for backfill");
			let response = services()
				.sending
				.send_federation_request(
					&backfill_server,
					federation::backfill::get_backfill::v1::Request {
						room_id: room_id.to_owned(),
						v: vec![first_pdu.1.event_id.as_ref().to_owned()],
						limit: uint!(100),
					},
				)
				.await;
			match response {
				Ok(response) => {
					let pub_key_map = RwLock::new(BTreeMap::new());
					for pdu in response.pdus {
						if let Err(e) = self.backfill_pdu(&backfill_server, pdu, &pub_key_map).await {
							warn!("Failed to add backfilled pdu in room {room_id}: {e}");
						}
					}
					return Ok(());
				},
				Err(e) => {
					warn!("{backfill_server} failed to provide backfill for room {room_id}: {e}");
				},
			}
		}

		info!("No servers could backfill, but backfill was needed in room {room_id}");
		Ok(())
	}

	#[tracing::instrument(skip(self, pdu, pub_key_map))]
	pub async fn backfill_pdu(
		&self, origin: &ServerName, pdu: Box<RawJsonValue>,
		pub_key_map: &RwLock<BTreeMap<String, BTreeMap<String, Base64>>>,
	) -> Result<()> {
		let (event_id, value, room_id) = parse_incoming_pdu(&pdu)?;

		// Lock so we cannot backfill the same pdu twice at the same time
		let mutex_lock = services()
			.rooms
			.event_handler
			.mutex_federation
			.lock(&room_id)
			.await;

		// Skip the PDU if we already have it as a timeline event
		if let Some(pdu_id) = self.get_pdu_id(&event_id)? {
			let pdu_id = pdu_id.to_vec();
			debug!("We already know {event_id} at {pdu_id:?}");
			return Ok(());
		}

		services()
			.rooms
			.event_handler
			.fetch_required_signing_keys([&value], pub_key_map)
			.await?;

		services()
			.rooms
			.event_handler
			.handle_incoming_pdu(origin, &room_id, &event_id, value, false, pub_key_map)
			.await?;

		let value = self.get_pdu_json(&event_id)?.expect("We just created it");
		let pdu = self.get_pdu(&event_id)?.expect("We just created it");

		let shortroomid = services()
			.rooms
			.short
			.get_shortroomid(&room_id)?
			.expect("room exists");

		let insert_lock = self.mutex_insert.lock(&room_id).await;

		let max = u64::MAX;
		let count = services().globals.next_count()?;
		let mut pdu_id = shortroomid.to_be_bytes().to_vec();
		pdu_id.extend_from_slice(&0_u64.to_be_bytes());
		pdu_id.extend_from_slice(&(validated!(max - count)?).to_be_bytes());

		// Insert pdu
		self.db.prepend_backfill_pdu(&pdu_id, &event_id, &value)?;

		drop(insert_lock);

		if pdu.kind == TimelineEventType::RoomMessage {
			let content = serde_json::from_str::<ExtractBody>(pdu.content.get())
				.map_err(|_| Error::bad_database("Invalid content in pdu."))?;

			if let Some(body) = content.body {
				services()
					.rooms
					.search
					.index_pdu(shortroomid, &pdu_id, &body)?;
			}
		}
		drop(mutex_lock);

		debug!("Prepended backfill pdu");
		Ok(())
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn comparisons() {
		assert!(PduCount::Normal(1) < PduCount::Normal(2));
		assert!(PduCount::Backfilled(2) < PduCount::Backfilled(1));
		assert!(PduCount::Normal(1) > PduCount::Backfilled(1));
		assert!(PduCount::Backfilled(1) < PduCount::Normal(1));
	}
}
