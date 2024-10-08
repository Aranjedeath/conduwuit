use std::{mem::size_of, sync::Arc};

use conduit::{utils, Error, Result};
use database::Map;
use ruma::{events::receipt::ReceiptEvent, serde::Raw, CanonicalJsonObject, RoomId, UserId};

use super::AnySyncEphemeralRoomEventIter;
use crate::{globals, Dep};

pub(super) struct Data {
	roomuserid_privateread: Arc<Map>,
	roomuserid_lastprivatereadupdate: Arc<Map>,
	services: Services,
	readreceiptid_readreceipt: Arc<Map>,
}

struct Services {
	globals: Dep<globals::Service>,
}

impl Data {
	pub(super) fn new(args: &crate::Args<'_>) -> Self {
		let db = &args.db;
		Self {
			roomuserid_privateread: db["roomuserid_privateread"].clone(),
			roomuserid_lastprivatereadupdate: db["roomuserid_lastprivatereadupdate"].clone(),
			readreceiptid_readreceipt: db["readreceiptid_readreceipt"].clone(),
			services: Services {
				globals: args.depend::<globals::Service>("globals"),
			},
		}
	}

	pub(super) fn readreceipt_update(&self, user_id: &UserId, room_id: &RoomId, event: &ReceiptEvent) -> Result<()> {
		let mut prefix = room_id.as_bytes().to_vec();
		prefix.push(0xFF);

		let mut last_possible_key = prefix.clone();
		last_possible_key.extend_from_slice(&u64::MAX.to_be_bytes());

		// Remove old entry
		if let Some((old, _)) = self
			.readreceiptid_readreceipt
			.iter_from(&last_possible_key, true)
			.take_while(|(key, _)| key.starts_with(&prefix))
			.find(|(key, _)| {
				key.rsplit(|&b| b == 0xFF)
					.next()
					.expect("rsplit always returns an element")
					== user_id.as_bytes()
			}) {
			// This is the old room_latest
			self.readreceiptid_readreceipt.remove(&old)?;
		}

		let mut room_latest_id = prefix;
		room_latest_id.extend_from_slice(&self.services.globals.next_count()?.to_be_bytes());
		room_latest_id.push(0xFF);
		room_latest_id.extend_from_slice(user_id.as_bytes());

		self.readreceiptid_readreceipt.insert(
			&room_latest_id,
			&serde_json::to_vec(event).expect("EduEvent::to_string always works"),
		)?;

		Ok(())
	}

	pub(super) fn readreceipts_since<'a>(&'a self, room_id: &RoomId, since: u64) -> AnySyncEphemeralRoomEventIter<'a> {
		let mut prefix = room_id.as_bytes().to_vec();
		prefix.push(0xFF);
		let prefix2 = prefix.clone();

		let mut first_possible_edu = prefix.clone();
		first_possible_edu.extend_from_slice(&(since.saturating_add(1)).to_be_bytes()); // +1 so we don't send the event at since

		Box::new(
			self.readreceiptid_readreceipt
				.iter_from(&first_possible_edu, false)
				.take_while(move |(k, _)| k.starts_with(&prefix2))
				.map(move |(k, v)| {
					let count_offset = prefix.len().saturating_add(size_of::<u64>());
					let count = utils::u64_from_bytes(&k[prefix.len()..count_offset])
						.map_err(|_| Error::bad_database("Invalid readreceiptid count in db."))?;
					let user_id_offset = count_offset.saturating_add(1);
					let user_id = UserId::parse(
						utils::string_from_bytes(&k[user_id_offset..])
							.map_err(|_| Error::bad_database("Invalid readreceiptid userid bytes in db."))?,
					)
					.map_err(|_| Error::bad_database("Invalid readreceiptid userid in db."))?;

					let mut json = serde_json::from_slice::<CanonicalJsonObject>(&v)
						.map_err(|_| Error::bad_database("Read receipt in roomlatestid_roomlatest is invalid json."))?;
					json.remove("room_id");

					Ok((
						user_id,
						count,
						Raw::from_json(serde_json::value::to_raw_value(&json).expect("json is valid raw value")),
					))
				}),
		)
	}

	pub(super) fn private_read_set(&self, room_id: &RoomId, user_id: &UserId, count: u64) -> Result<()> {
		let mut key = room_id.as_bytes().to_vec();
		key.push(0xFF);
		key.extend_from_slice(user_id.as_bytes());

		self.roomuserid_privateread
			.insert(&key, &count.to_be_bytes())?;

		self.roomuserid_lastprivatereadupdate
			.insert(&key, &self.services.globals.next_count()?.to_be_bytes())
	}

	pub(super) fn private_read_get(&self, room_id: &RoomId, user_id: &UserId) -> Result<Option<u64>> {
		let mut key = room_id.as_bytes().to_vec();
		key.push(0xFF);
		key.extend_from_slice(user_id.as_bytes());

		self.roomuserid_privateread
			.get(&key)?
			.map_or(Ok(None), |v| {
				Ok(Some(
					utils::u64_from_bytes(&v).map_err(|_| Error::bad_database("Invalid private read marker bytes"))?,
				))
			})
	}

	pub(super) fn last_privateread_update(&self, user_id: &UserId, room_id: &RoomId) -> Result<u64> {
		let mut key = room_id.as_bytes().to_vec();
		key.push(0xFF);
		key.extend_from_slice(user_id.as_bytes());

		Ok(self
			.roomuserid_lastprivatereadupdate
			.get(&key)?
			.map(|bytes| {
				utils::u64_from_bytes(&bytes)
					.map_err(|_| Error::bad_database("Count in roomuserid_lastprivatereadupdate is invalid."))
			})
			.transpose()?
			.unwrap_or(0))
	}
}
