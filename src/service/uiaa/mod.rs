mod data;

use std::sync::Arc;

use conduit::{utils, utils::hash, Error, Result};
use data::Data;
use ruma::{
	api::client::{
		error::ErrorKind,
		uiaa::{AuthData, AuthType, Password, UiaaInfo, UserIdentifier},
	},
	CanonicalJsonValue, DeviceId, UserId,
};
use tracing::error;

use crate::services;

pub const SESSION_ID_LENGTH: usize = 32;

pub struct Service {
	pub db: Data,
}

impl crate::Service for Service {
	fn build(args: crate::Args<'_>) -> Result<Arc<Self>> {
		Ok(Arc::new(Self {
			db: Data::new(args.db),
		}))
	}

	fn name(&self) -> &str { crate::service::make_name(std::module_path!()) }
}

impl Service {
	/// Creates a new Uiaa session. Make sure the session token is unique.
	pub fn create(
		&self, user_id: &UserId, device_id: &DeviceId, uiaainfo: &UiaaInfo, json_body: &CanonicalJsonValue,
	) -> Result<()> {
		self.db.set_uiaa_request(
			user_id,
			device_id,
			uiaainfo.session.as_ref().expect("session should be set"), /* TODO: better session error handling (why
			                                                            * is it optional in ruma?) */
			json_body,
		)?;
		self.db.update_uiaa_session(
			user_id,
			device_id,
			uiaainfo.session.as_ref().expect("session should be set"),
			Some(uiaainfo),
		)
	}

	pub fn try_auth(
		&self, user_id: &UserId, device_id: &DeviceId, auth: &AuthData, uiaainfo: &UiaaInfo,
	) -> Result<(bool, UiaaInfo)> {
		let mut uiaainfo = auth.session().map_or_else(
			|| Ok(uiaainfo.clone()),
			|session| self.db.get_uiaa_session(user_id, device_id, session),
		)?;

		if uiaainfo.session.is_none() {
			uiaainfo.session = Some(utils::random_string(SESSION_ID_LENGTH));
		}

		match auth {
			// Find out what the user completed
			AuthData::Password(Password {
				identifier,
				password,
				#[cfg(feature = "element_hacks")]
				user,
				..
			}) => {
				#[cfg(feature = "element_hacks")]
				let username = if let Some(UserIdentifier::UserIdOrLocalpart(username)) = identifier {
					username
				} else if let Some(username) = user {
					username
				} else {
					return Err(Error::BadRequest(ErrorKind::Unrecognized, "Identifier type not recognized."));
				};

				#[cfg(not(feature = "element_hacks"))]
				let Some(UserIdentifier::UserIdOrLocalpart(username)) = identifier
				else {
					return Err(Error::BadRequest(ErrorKind::Unrecognized, "Identifier type not recognized."));
				};

				let user_id = UserId::parse_with_server_name(username.clone(), services().globals.server_name())
					.map_err(|_| Error::BadRequest(ErrorKind::InvalidParam, "User ID is invalid."))?;

				// Check if password is correct
				if let Some(hash) = services().users.password_hash(&user_id)? {
					let hash_matches = hash::verify_password(password, &hash).is_ok();
					if !hash_matches {
						uiaainfo.auth_error = Some(ruma::api::client::error::StandardErrorBody {
							kind: ErrorKind::forbidden(),
							message: "Invalid username or password.".to_owned(),
						});
						return Ok((false, uiaainfo));
					}
				}

				// Password was correct! Let's add it to `completed`
				uiaainfo.completed.push(AuthType::Password);
			},
			AuthData::RegistrationToken(t) => {
				if Some(t.token.trim()) == services().globals.config.registration_token.as_deref() {
					uiaainfo.completed.push(AuthType::RegistrationToken);
				} else {
					uiaainfo.auth_error = Some(ruma::api::client::error::StandardErrorBody {
						kind: ErrorKind::forbidden(),
						message: "Invalid registration token.".to_owned(),
					});
					return Ok((false, uiaainfo));
				}
			},
			AuthData::Dummy(_) => {
				uiaainfo.completed.push(AuthType::Dummy);
			},
			k => error!("type not supported: {:?}", k),
		}

		// Check if a flow now succeeds
		let mut completed = false;
		'flows: for flow in &mut uiaainfo.flows {
			for stage in &flow.stages {
				if !uiaainfo.completed.contains(stage) {
					continue 'flows;
				}
			}
			// We didn't break, so this flow succeeded!
			completed = true;
		}

		if !completed {
			self.db.update_uiaa_session(
				user_id,
				device_id,
				uiaainfo.session.as_ref().expect("session is always set"),
				Some(&uiaainfo),
			)?;
			return Ok((false, uiaainfo));
		}

		// UIAA was successful! Remove this session and return true
		self.db.update_uiaa_session(
			user_id,
			device_id,
			uiaainfo.session.as_ref().expect("session is always set"),
			None,
		)?;
		Ok((true, uiaainfo))
	}

	#[must_use]
	pub fn get_uiaa_request(
		&self, user_id: &UserId, device_id: &DeviceId, session: &str,
	) -> Option<CanonicalJsonValue> {
		self.db.get_uiaa_request(user_id, device_id, session)
	}
}
