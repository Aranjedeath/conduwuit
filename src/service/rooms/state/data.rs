use std::{collections::HashSet, sync::Arc};

use conduit::{utils, Error, Result};
use database::{Database, Map};
use ruma::{EventId, OwnedEventId, RoomId};

use super::RoomMutexGuard;

pub(super) struct Data {
	shorteventid_shortstatehash: Arc<Map>,
	roomid_pduleaves: Arc<Map>,
	roomid_shortstatehash: Arc<Map>,
}

impl Data {
	pub(super) fn new(db: &Arc<Database>) -> Self {
		Self {
			shorteventid_shortstatehash: db["shorteventid_shortstatehash"].clone(),
			roomid_pduleaves: db["roomid_pduleaves"].clone(),
			roomid_shortstatehash: db["roomid_shortstatehash"].clone(),
		}
	}

	pub(super) fn get_room_shortstatehash(&self, room_id: &RoomId) -> Result<Option<u64>> {
		self.roomid_shortstatehash
			.get(room_id.as_bytes())?
			.map_or(Ok(None), |bytes| {
				Ok(Some(utils::u64_from_bytes(&bytes).map_err(|_| {
					Error::bad_database("Invalid shortstatehash in roomid_shortstatehash")
				})?))
			})
	}

	#[inline]
	pub(super) fn set_room_state(
		&self,
		room_id: &RoomId,
		new_shortstatehash: u64,
		_mutex_lock: &RoomMutexGuard, // Take mutex guard to make sure users get the room state mutex
	) -> Result<()> {
		self.roomid_shortstatehash
			.insert(room_id.as_bytes(), &new_shortstatehash.to_be_bytes())?;
		Ok(())
	}

	pub(super) fn set_event_state(&self, shorteventid: u64, shortstatehash: u64) -> Result<()> {
		self.shorteventid_shortstatehash
			.insert(&shorteventid.to_be_bytes(), &shortstatehash.to_be_bytes())?;
		Ok(())
	}

	pub(super) fn get_forward_extremities(&self, room_id: &RoomId) -> Result<HashSet<Arc<EventId>>> {
		let mut prefix = room_id.as_bytes().to_vec();
		prefix.push(0xFF);

		self.roomid_pduleaves
			.scan_prefix(prefix)
			.map(|(_, bytes)| {
				EventId::parse_arc(
					utils::string_from_bytes(&bytes)
						.map_err(|_| Error::bad_database("EventID in roomid_pduleaves is invalid unicode."))?,
				)
				.map_err(|_| Error::bad_database("EventId in roomid_pduleaves is invalid."))
			})
			.collect()
	}

	pub(super) fn set_forward_extremities(
		&self,
		room_id: &RoomId,
		event_ids: Vec<OwnedEventId>,
		_mutex_lock: &RoomMutexGuard, // Take mutex guard to make sure users get the room state mutex
	) -> Result<()> {
		let mut prefix = room_id.as_bytes().to_vec();
		prefix.push(0xFF);

		for (key, _) in self.roomid_pduleaves.scan_prefix(prefix.clone()) {
			self.roomid_pduleaves.remove(&key)?;
		}

		for event_id in event_ids {
			let mut key = prefix.clone();
			key.extend_from_slice(event_id.as_bytes());
			self.roomid_pduleaves.insert(&key, event_id.as_bytes())?;
		}

		Ok(())
	}
}
