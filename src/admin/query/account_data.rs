use clap::Subcommand;
use conduwuit::Result;
use futures::StreamExt;
use ruma::{RoomId, UserId, events::room::message::RoomMessageEventContent};

use crate::{admin_command, admin_command_dispatch};

#[admin_command_dispatch]
#[derive(Debug, Subcommand)]
/// All the getters and iterators from src/database/key_value/account_data.rs
pub(crate) enum AccountDataCommand {
	/// - Returns all changes to the account data that happened after `since`.
	ChangesSince {
		/// Full user ID
		user_id: Box<UserId>,
		/// UNIX timestamp since (u64)
		since: u64,
		/// Optional room ID of the account data
		room_id: Option<Box<RoomId>>,
	},

	/// - Searches the account data for a specific kind.
	AccountDataGet {
		/// Full user ID
		user_id: Box<UserId>,
		/// Account data event type
		kind: String,
		/// Optional room ID of the account data
		room_id: Option<Box<RoomId>>,
	},
}

#[admin_command]
async fn changes_since(
	&self,
	user_id: Box<UserId>,
	since: u64,
	room_id: Option<Box<RoomId>>,
) -> Result<RoomMessageEventContent> {
	let timer = tokio::time::Instant::now();
	let results: Vec<_> = self
		.services
		.account_data
		.changes_since(room_id.as_deref(), &user_id, since, None)
		.collect()
		.await;
	let query_time = timer.elapsed();

	Ok(RoomMessageEventContent::notice_markdown(format!(
		"Query completed in {query_time:?}:\n\n```rs\n{results:#?}\n```"
	)))
}

#[admin_command]
async fn account_data_get(
	&self,
	user_id: Box<UserId>,
	kind: String,
	room_id: Option<Box<RoomId>>,
) -> Result<RoomMessageEventContent> {
	let timer = tokio::time::Instant::now();
	let results = self
		.services
		.account_data
		.get_raw(room_id.as_deref(), &user_id, &kind)
		.await;
	let query_time = timer.elapsed();

	Ok(RoomMessageEventContent::notice_markdown(format!(
		"Query completed in {query_time:?}:\n\n```rs\n{results:#?}\n```"
	)))
}
