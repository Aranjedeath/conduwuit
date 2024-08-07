use std::{collections::BTreeMap, fmt::Write as _};

use api::client::{join_room_by_id_helper, leave_all_rooms, update_avatar_url, update_displayname};
use conduit::{utils, Result};
use ruma::{
	events::{
		room::message::RoomMessageEventContent,
		tag::{TagEvent, TagEventContent, TagInfo},
		RoomAccountDataEventType,
	},
	OwnedRoomId, OwnedRoomOrAliasId, OwnedUserId, RoomId,
};
use tracing::{error, info, warn};

use crate::{
	escape_html, get_room_info, services,
	utils::{parse_active_local_user_id, parse_local_user_id},
};

const AUTO_GEN_PASSWORD_LENGTH: usize = 25;

pub(super) async fn list(_body: Vec<&str>) -> Result<RoomMessageEventContent> {
	match services().users.list_local_users() {
		Ok(users) => {
			let mut plain_msg = format!("Found {} local user account(s):\n```\n", users.len());
			plain_msg += users.join("\n").as_str();
			plain_msg += "\n```";

			Ok(RoomMessageEventContent::notice_markdown(plain_msg))
		},
		Err(e) => Ok(RoomMessageEventContent::text_plain(e.to_string())),
	}
}

pub(super) async fn create(
	_body: Vec<&str>, username: String, password: Option<String>,
) -> Result<RoomMessageEventContent> {
	// Validate user id
	let user_id = parse_local_user_id(&username)?;

	if services().users.exists(&user_id)? {
		return Ok(RoomMessageEventContent::text_plain(format!("Userid {user_id} already exists")));
	}

	if user_id.is_historical() {
		return Ok(RoomMessageEventContent::text_plain(format!(
			"User ID {user_id} does not conform to new Matrix identifier spec"
		)));
	}

	let password = password.unwrap_or_else(|| utils::random_string(AUTO_GEN_PASSWORD_LENGTH));

	// Create user
	services().users.create(&user_id, Some(password.as_str()))?;

	// Default to pretty displayname
	let mut displayname = user_id.localpart().to_owned();

	// If `new_user_displayname_suffix` is set, registration will push whatever
	// content is set to the user's display name with a space before it
	if !services()
		.globals
		.config
		.new_user_displayname_suffix
		.is_empty()
	{
		write!(displayname, " {}", services().globals.config.new_user_displayname_suffix)
			.expect("should be able to write to string buffer");
	}

	services()
		.users
		.set_displayname(&user_id, Some(displayname))
		.await?;

	// Initial account data
	services().account_data.update(
		None,
		&user_id,
		ruma::events::GlobalAccountDataEventType::PushRules
			.to_string()
			.into(),
		&serde_json::to_value(ruma::events::push_rules::PushRulesEvent {
			content: ruma::events::push_rules::PushRulesEventContent {
				global: ruma::push::Ruleset::server_default(&user_id),
			},
		})
		.expect("to json value always works"),
	)?;

	if !services().globals.config.auto_join_rooms.is_empty() {
		for room in &services().globals.config.auto_join_rooms {
			if !services()
				.rooms
				.state_cache
				.server_in_room(services().globals.server_name(), room)?
			{
				warn!("Skipping room {room} to automatically join as we have never joined before.");
				continue;
			}

			if let Some(room_id_server_name) = room.server_name() {
				match join_room_by_id_helper(
					&user_id,
					room,
					Some("Automatically joining this room upon registration".to_owned()),
					&[room_id_server_name.to_owned(), services().globals.server_name().to_owned()],
					None,
				)
				.await
				{
					Ok(_response) => {
						info!("Automatically joined room {room} for user {user_id}");
					},
					Err(e) => {
						// don't return this error so we don't fail registrations
						error!("Failed to automatically join room {room} for user {user_id}: {e}");
					},
				};
			}
		}
	}

	// we dont add a device since we're not the user, just the creator

	// Inhibit login does not work for guests
	Ok(RoomMessageEventContent::text_plain(format!(
		"Created user with user_id: {user_id} and password: `{password}`"
	)))
}

pub(super) async fn deactivate(
	_body: Vec<&str>, no_leave_rooms: bool, user_id: String,
) -> Result<RoomMessageEventContent> {
	// Validate user id
	let user_id = parse_local_user_id(&user_id)?;

	// don't deactivate the server service account
	if user_id == services().globals.server_user {
		return Ok(RoomMessageEventContent::text_plain(
			"Not allowed to deactivate the server service account.",
		));
	}

	services().users.deactivate_account(&user_id)?;

	if !no_leave_rooms {
		services()
			.admin
			.send_message(RoomMessageEventContent::text_plain(format!(
				"Making {user_id} leave all rooms after deactivation..."
			)))
			.await;

		let all_joined_rooms: Vec<OwnedRoomId> = services()
			.rooms
			.state_cache
			.rooms_joined(&user_id)
			.filter_map(Result::ok)
			.collect();
		update_displayname(user_id.clone(), None, all_joined_rooms.clone()).await?;
		update_avatar_url(user_id.clone(), None, None, all_joined_rooms).await?;
		leave_all_rooms(&user_id).await;
	}

	Ok(RoomMessageEventContent::text_plain(format!(
		"User {user_id} has been deactivated"
	)))
}

pub(super) async fn reset_password(_body: Vec<&str>, username: String) -> Result<RoomMessageEventContent> {
	let user_id = parse_local_user_id(&username)?;

	if user_id == services().globals.server_user {
		return Ok(RoomMessageEventContent::text_plain(
			"Not allowed to set the password for the server account. Please use the emergency password config option.",
		));
	}

	let new_password = utils::random_string(AUTO_GEN_PASSWORD_LENGTH);

	match services()
		.users
		.set_password(&user_id, Some(new_password.as_str()))
	{
		Ok(()) => Ok(RoomMessageEventContent::text_plain(format!(
			"Successfully reset the password for user {user_id}: `{new_password}`"
		))),
		Err(e) => Ok(RoomMessageEventContent::text_plain(format!(
			"Couldn't reset the password for user {user_id}: {e}"
		))),
	}
}

pub(super) async fn deactivate_all(
	body: Vec<&str>, no_leave_rooms: bool, force: bool,
) -> Result<RoomMessageEventContent> {
	if body.len() < 2 || !body[0].trim().starts_with("```") || body.last().unwrap_or(&"").trim() != "```" {
		return Ok(RoomMessageEventContent::text_plain(
			"Expected code block in command body. Add --help for details.",
		));
	}

	let usernames = body
		.clone()
		.drain(1..body.len().saturating_sub(1))
		.collect::<Vec<_>>();

	let mut user_ids: Vec<OwnedUserId> = Vec::with_capacity(usernames.len());
	let mut admins = Vec::new();

	for username in usernames {
		match parse_active_local_user_id(username) {
			Ok(user_id) => {
				if services().users.is_admin(&user_id)? && !force {
					services()
						.admin
						.send_message(RoomMessageEventContent::text_plain(format!(
							"{username} is an admin and --force is not set, skipping over"
						)))
						.await;
					admins.push(username);
					continue;
				}

				// don't deactivate the server service account
				if user_id == services().globals.server_user {
					services()
						.admin
						.send_message(RoomMessageEventContent::text_plain(format!(
							"{username} is the server service account, skipping over"
						)))
						.await;
					continue;
				}

				user_ids.push(user_id);
			},
			Err(e) => {
				services()
					.admin
					.send_message(RoomMessageEventContent::text_plain(format!(
						"{username} is not a valid username, skipping over: {e}"
					)))
					.await;
				continue;
			},
		}
	}

	let mut deactivation_count: usize = 0;

	for user_id in user_ids {
		match services().users.deactivate_account(&user_id) {
			Ok(()) => {
				deactivation_count = deactivation_count.saturating_add(1);
				if !no_leave_rooms {
					info!("Forcing user {user_id} to leave all rooms apart of deactivate-all");
					let all_joined_rooms: Vec<OwnedRoomId> = services()
						.rooms
						.state_cache
						.rooms_joined(&user_id)
						.filter_map(Result::ok)
						.collect();
					update_displayname(user_id.clone(), None, all_joined_rooms.clone()).await?;
					update_avatar_url(user_id.clone(), None, None, all_joined_rooms).await?;
					leave_all_rooms(&user_id).await;
				}
			},
			Err(e) => {
				services()
					.admin
					.send_message(RoomMessageEventContent::text_plain(format!("Failed deactivating user: {e}")))
					.await;
			},
		}
	}

	if admins.is_empty() {
		Ok(RoomMessageEventContent::text_plain(format!(
			"Deactivated {deactivation_count} accounts."
		)))
	} else {
		Ok(RoomMessageEventContent::text_plain(format!(
			"Deactivated {deactivation_count} accounts.\nSkipped admin accounts: {}. Use --force to deactivate admin \
			 accounts",
			admins.join(", ")
		)))
	}
}

pub(super) async fn list_joined_rooms(_body: Vec<&str>, user_id: String) -> Result<RoomMessageEventContent> {
	// Validate user id
	let user_id = parse_local_user_id(&user_id)?;

	let mut rooms: Vec<(OwnedRoomId, u64, String)> = services()
		.rooms
		.state_cache
		.rooms_joined(&user_id)
		.filter_map(Result::ok)
		.map(|room_id| get_room_info(&room_id))
		.collect();

	if rooms.is_empty() {
		return Ok(RoomMessageEventContent::text_plain("User is not in any rooms."));
	}

	rooms.sort_by_key(|r| r.1);
	rooms.reverse();

	let output_plain = format!(
		"Rooms {user_id} Joined ({}):\n{}",
		rooms.len(),
		rooms
			.iter()
			.map(|(id, members, name)| format!("{id}\tMembers: {members}\tName: {name}"))
			.collect::<Vec<_>>()
			.join("\n")
	);

	let output_html = format!(
		"<table><caption>Rooms {user_id} Joined \
		 ({})</caption>\n<tr><th>id</th>\t<th>members</th>\t<th>name</th></tr>\n{}</table>",
		rooms.len(),
		rooms
			.iter()
			.fold(String::new(), |mut output, (id, members, name)| {
				writeln!(
					output,
					"<tr><td>{}</td>\t<td>{}</td>\t<td>{}</td></tr>",
					escape_html(id.as_ref()),
					members,
					escape_html(name)
				)
				.unwrap();
				output
			})
	);

	Ok(RoomMessageEventContent::text_html(output_plain, output_html))
}

pub(super) async fn force_join_room(
	_body: Vec<&str>, user_id: String, room_id: OwnedRoomOrAliasId,
) -> Result<RoomMessageEventContent> {
	let user_id = parse_local_user_id(&user_id)?;
	let room_id = services().rooms.alias.resolve(&room_id).await?;

	assert!(service::user_is_local(&user_id), "Parsed user_id must be a local user");
	join_room_by_id_helper(&user_id, &room_id, None, &[], None).await?;

	Ok(RoomMessageEventContent::notice_markdown(format!(
		"{user_id} has been joined to {room_id}.",
	)))
}

pub(super) async fn make_user_admin(_body: Vec<&str>, user_id: String) -> Result<RoomMessageEventContent> {
	let user_id = parse_local_user_id(&user_id)?;
	let displayname = services()
		.users
		.displayname(&user_id)?
		.unwrap_or_else(|| user_id.to_string());

	assert!(service::user_is_local(&user_id), "Parsed user_id must be a local user");
	service::admin::make_user_admin(&user_id, displayname).await?;

	Ok(RoomMessageEventContent::notice_markdown(format!(
		"{user_id} has been granted admin privileges.",
	)))
}

pub(super) async fn put_room_tag(
	_body: Vec<&str>, user_id: String, room_id: Box<RoomId>, tag: String,
) -> Result<RoomMessageEventContent> {
	let user_id = parse_active_local_user_id(&user_id)?;

	let event = services()
		.account_data
		.get(Some(&room_id), &user_id, RoomAccountDataEventType::Tag)?;

	let mut tags_event = event.map_or_else(
		|| TagEvent {
			content: TagEventContent {
				tags: BTreeMap::new(),
			},
		},
		|e| serde_json::from_str(e.get()).expect("Bad account data in database for user {user_id}"),
	);

	tags_event
		.content
		.tags
		.insert(tag.clone().into(), TagInfo::new());

	services().account_data.update(
		Some(&room_id),
		&user_id,
		RoomAccountDataEventType::Tag,
		&serde_json::to_value(tags_event).expect("to json value always works"),
	)?;

	Ok(RoomMessageEventContent::text_plain(format!(
		"Successfully updated room account data for {user_id} and room {room_id} with tag {tag}"
	)))
}

pub(super) async fn delete_room_tag(
	_body: Vec<&str>, user_id: String, room_id: Box<RoomId>, tag: String,
) -> Result<RoomMessageEventContent> {
	let user_id = parse_active_local_user_id(&user_id)?;

	let event = services()
		.account_data
		.get(Some(&room_id), &user_id, RoomAccountDataEventType::Tag)?;

	let mut tags_event = event.map_or_else(
		|| TagEvent {
			content: TagEventContent {
				tags: BTreeMap::new(),
			},
		},
		|e| serde_json::from_str(e.get()).expect("Bad account data in database for user {user_id}"),
	);

	tags_event.content.tags.remove(&tag.clone().into());

	services().account_data.update(
		Some(&room_id),
		&user_id,
		RoomAccountDataEventType::Tag,
		&serde_json::to_value(tags_event).expect("to json value always works"),
	)?;

	Ok(RoomMessageEventContent::text_plain(format!(
		"Successfully updated room account data for {user_id} and room {room_id}, deleting room tag {tag}"
	)))
}

pub(super) async fn get_room_tags(
	_body: Vec<&str>, user_id: String, room_id: Box<RoomId>,
) -> Result<RoomMessageEventContent> {
	let user_id = parse_active_local_user_id(&user_id)?;

	let event = services()
		.account_data
		.get(Some(&room_id), &user_id, RoomAccountDataEventType::Tag)?;

	let tags_event = event.map_or_else(
		|| TagEvent {
			content: TagEventContent {
				tags: BTreeMap::new(),
			},
		},
		|e| serde_json::from_str(e.get()).expect("Bad account data in database for user {user_id}"),
	);

	Ok(RoomMessageEventContent::notice_markdown(format!(
		"```\n{:#?}\n```",
		tags_event.content.tags
	)))
}
