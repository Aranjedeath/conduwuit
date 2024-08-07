mod account_data;
mod appservice;
mod globals;
mod presence;
mod resolver;
mod room_alias;
mod room_state_cache;
mod sending;
mod users;

use clap::Subcommand;
use conduit::Result;
use room_state_cache::room_state_cache;
use ruma::{
	events::{room::message::RoomMessageEventContent, RoomAccountDataEventType},
	OwnedServerName, RoomAliasId, RoomId, ServerName, UserId,
};

use self::{
	account_data::account_data, appservice::appservice, globals::globals, presence::presence, resolver::resolver,
	room_alias::room_alias, sending::sending, users::users,
};

#[cfg_attr(test, derive(Debug))]
#[derive(Subcommand)]
/// Query tables from database
pub(super) enum QueryCommand {
	/// - account_data.rs iterators and getters
	#[command(subcommand)]
	AccountData(AccountData),

	/// - appservice.rs iterators and getters
	#[command(subcommand)]
	Appservice(Appservice),

	/// - presence.rs iterators and getters
	#[command(subcommand)]
	Presence(Presence),

	/// - rooms/alias.rs iterators and getters
	#[command(subcommand)]
	RoomAlias(RoomAlias),

	/// - rooms/state_cache iterators and getters
	#[command(subcommand)]
	RoomStateCache(RoomStateCache),

	/// - globals.rs iterators and getters
	#[command(subcommand)]
	Globals(Globals),

	/// - sending.rs iterators and getters
	#[command(subcommand)]
	Sending(Sending),

	/// - users.rs iterators and getters
	#[command(subcommand)]
	Users(Users),

	/// - resolver service
	#[command(subcommand)]
	Resolver(Resolver),
}

#[cfg_attr(test, derive(Debug))]
#[derive(Subcommand)]
/// All the getters and iterators from src/database/key_value/account_data.rs
pub(super) enum AccountData {
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
	Get {
		/// Full user ID
		user_id: Box<UserId>,
		/// Account data event type
		kind: RoomAccountDataEventType,
		/// Optional room ID of the account data
		room_id: Option<Box<RoomId>>,
	},
}

#[cfg_attr(test, derive(Debug))]
#[derive(Subcommand)]
/// All the getters and iterators from src/database/key_value/appservice.rs
pub(super) enum Appservice {
	/// - Gets the appservice registration info/details from the ID as a string
	GetRegistration {
		/// Appservice registration ID
		appservice_id: Box<str>,
	},

	/// - Gets all appservice registrations with their ID and registration info
	All,
}

#[cfg_attr(test, derive(Debug))]
#[derive(Subcommand)]
/// All the getters and iterators from src/database/key_value/presence.rs
pub(super) enum Presence {
	/// - Returns the latest presence event for the given user.
	GetPresence {
		/// Full user ID
		user_id: Box<UserId>,
	},

	/// - Iterator of the most recent presence updates that happened after the
	///   event with id `since`.
	PresenceSince {
		/// UNIX timestamp since (u64)
		since: u64,
	},
}

#[cfg_attr(test, derive(Debug))]
#[derive(Subcommand)]
/// All the getters and iterators from src/database/key_value/rooms/alias.rs
pub(super) enum RoomAlias {
	ResolveLocalAlias {
		/// Full room alias
		alias: Box<RoomAliasId>,
	},

	/// - Iterator of all our local room aliases for the room ID
	LocalAliasesForRoom {
		/// Full room ID
		room_id: Box<RoomId>,
	},

	/// - Iterator of all our local aliases in our database with their room IDs
	AllLocalAliases,
}

#[cfg_attr(test, derive(Debug))]
#[derive(Subcommand)]
pub(super) enum RoomStateCache {
	ServerInRoom {
		server: Box<ServerName>,
		room_id: Box<RoomId>,
	},

	RoomServers {
		room_id: Box<RoomId>,
	},

	ServerRooms {
		server: Box<ServerName>,
	},

	RoomMembers {
		room_id: Box<RoomId>,
	},

	LocalUsersInRoom {
		room_id: Box<RoomId>,
	},

	ActiveLocalUsersInRoom {
		room_id: Box<RoomId>,
	},

	RoomJoinedCount {
		room_id: Box<RoomId>,
	},

	RoomInvitedCount {
		room_id: Box<RoomId>,
	},

	RoomUserOnceJoined {
		room_id: Box<RoomId>,
	},

	RoomMembersInvited {
		room_id: Box<RoomId>,
	},

	GetInviteCount {
		room_id: Box<RoomId>,
		user_id: Box<UserId>,
	},

	GetLeftCount {
		room_id: Box<RoomId>,
		user_id: Box<UserId>,
	},

	RoomsJoined {
		user_id: Box<UserId>,
	},

	RoomsLeft {
		user_id: Box<UserId>,
	},

	RoomsInvited {
		user_id: Box<UserId>,
	},

	InviteState {
		user_id: Box<UserId>,
		room_id: Box<RoomId>,
	},
}

#[cfg_attr(test, derive(Debug))]
#[derive(Subcommand)]
/// All the getters and iterators from src/database/key_value/globals.rs
pub(super) enum Globals {
	DatabaseVersion,

	CurrentCount,

	LastCheckForUpdatesId,

	LoadKeypair,

	/// - This returns an empty `Ok(BTreeMap<..>)` when there are no keys found
	///   for the server.
	SigningKeysFor {
		origin: Box<ServerName>,
	},
}

#[cfg_attr(test, derive(Debug))]
#[derive(Subcommand)]
/// All the getters and iterators from src/database/key_value/sending.rs
pub(super) enum Sending {
	/// - Queries database for all `servercurrentevent_data`
	ActiveRequests,

	/// - Queries database for `servercurrentevent_data` but for a specific
	///   destination
	///
	/// This command takes only *one* format of these arguments:
	///
	/// appservice_id
	/// server_name
	/// user_id AND push_key
	///
	/// See src/service/sending/mod.rs for the definition of the `Destination`
	/// enum
	ActiveRequestsFor {
		#[arg(short, long)]
		appservice_id: Option<String>,
		#[arg(short, long)]
		server_name: Option<Box<ServerName>>,
		#[arg(short, long)]
		user_id: Option<Box<UserId>>,
		#[arg(short, long)]
		push_key: Option<String>,
	},

	/// - Queries database for `servernameevent_data` which are the queued up
	///   requests that will eventually be sent
	///
	/// This command takes only *one* format of these arguments:
	///
	/// appservice_id
	/// server_name
	/// user_id AND push_key
	///
	/// See src/service/sending/mod.rs for the definition of the `Destination`
	/// enum
	QueuedRequests {
		#[arg(short, long)]
		appservice_id: Option<String>,
		#[arg(short, long)]
		server_name: Option<Box<ServerName>>,
		#[arg(short, long)]
		user_id: Option<Box<UserId>>,
		#[arg(short, long)]
		push_key: Option<String>,
	},

	GetLatestEduCount {
		server_name: Box<ServerName>,
	},
}

#[cfg_attr(test, derive(Debug))]
#[derive(Subcommand)]
/// All the getters and iterators from src/database/key_value/users.rs
pub(super) enum Users {
	Iter,
}

#[cfg_attr(test, derive(Debug))]
#[derive(Subcommand)]
/// Resolver service and caches
pub(super) enum Resolver {
	/// Query the destinations cache
	DestinationsCache {
		server_name: Option<OwnedServerName>,
	},

	/// Query the overrides cache
	OverridesCache {
		name: Option<String>,
	},
}

/// Processes admin query commands
pub(super) async fn process(command: QueryCommand, _body: Vec<&str>) -> Result<RoomMessageEventContent> {
	Ok(match command {
		QueryCommand::AccountData(command) => account_data(command).await?,
		QueryCommand::Appservice(command) => appservice(command).await?,
		QueryCommand::Presence(command) => presence(command).await?,
		QueryCommand::RoomAlias(command) => room_alias(command).await?,
		QueryCommand::RoomStateCache(command) => room_state_cache(command).await?,
		QueryCommand::Globals(command) => globals(command).await?,
		QueryCommand::Sending(command) => sending(command).await?,
		QueryCommand::Users(command) => users(command).await?,
		QueryCommand::Resolver(command) => resolver(command).await?,
	})
}
