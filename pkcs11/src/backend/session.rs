use std::collections::HashMap;

use cryptoki_sys::{
    CKA_ID, CKA_LABEL, CKR_ARGUMENTS_BAD, CKR_DEVICE_ERROR, CKR_OK, CKS_RO_PUBLIC_SESSION,
    CK_FLAGS, CK_OBJECT_HANDLE, CK_RV, CK_SESSION_HANDLE, CK_SLOT_ID, CK_STATE,
};
use log::error;
use openapi::apis::default_api;

use crate::config::device::Slot;

use super::{
    db::{
        self,
        attr::{CkRawAttr, CkRawAttrTemplate},
        Db, Object,
    },
    mechanism::Mechanism,
};

#[derive(Clone, Debug)]
pub struct SessionManager {
    pub sessions: HashMap<CK_SESSION_HANDLE, Session>,
    pub next_session_handle: CK_SESSION_HANDLE,
}

impl SessionManager {
    pub fn new() -> Self {
        Self {
            sessions: HashMap::new(),
            next_session_handle: 1,
        }
    }

    pub fn create_session(
        &mut self,
        slot_id: CK_SLOT_ID,
        slot: Slot,
        flags: CK_FLAGS,
    ) -> CK_SESSION_HANDLE {
        let session = Session::new(slot_id, slot, flags);
        let handle = self.next_session_handle;
        self.sessions.insert(handle, session);

        self.next_session_handle += 1;
        handle
    }

    pub fn get_session(&self, handle: CK_SESSION_HANDLE) -> Option<&Session> {
        self.sessions.get(&handle)
    }

    pub fn get_session_mut(&mut self, handle: CK_SESSION_HANDLE) -> Option<&mut Session> {
        self.sessions.get_mut(&handle)
    }

    pub fn delete_session(
        &mut self,
        handle: CK_SESSION_HANDLE,
    ) -> Option<(CK_SESSION_HANDLE, Session)> {
        self.sessions.remove_entry(&handle)
    }

    pub fn delete_all_slot_sessions(&mut self, slot_id: CK_SLOT_ID) {
        let mut deleted_sessions = Vec::new();
        self.sessions.iter().for_each(|(handle, session)| {
            if session.slot_id == slot_id {
                deleted_sessions.push(*handle);
            }
        });
        for handle in deleted_sessions.iter() {
            self.sessions.remove(handle);
        }
    }
}

#[derive(Clone, Debug)]
pub struct Session {
    pub slot_id: CK_SLOT_ID,
    slot: Slot,
    pub flags: CK_FLAGS,
    pub state: CK_STATE,
    pub device_error: CK_RV,
    pub fetched_all_keys: bool,
    pub db: Db,
    pub sign_ctx: Option<SignCtx>,
    pub encrypt_ctx: Option<EncryptCtx>,
    pub decrypt_ctx: Option<DecryptCtx>,
    pub enum_ctx: Option<EnumCtx>,
}

impl Session {
    pub fn new(slot_id: CK_SLOT_ID, slot: Slot, flags: CK_FLAGS) -> Self {
        Self {
            slot,
            slot_id,
            flags,
            state: CKS_RO_PUBLIC_SESSION,
            fetched_all_keys: false,
            db: Db::new(),
            device_error: CKR_OK,
            sign_ctx: None,
            encrypt_ctx: None,
            decrypt_ctx: None,
            enum_ctx: None,
        }
    }
    pub fn get_ck_info(&self) -> cryptoki_sys::CK_SESSION_INFO {
        cryptoki_sys::CK_SESSION_INFO {
            slotID: self.slot_id,
            state: self.state,
            flags: self.flags,
            ulDeviceError: self.device_error,
        }
    }

    pub fn enum_init(&mut self, template: Option<CkRawAttrTemplate>) -> CK_RV {
        if self.enum_ctx.is_some() {
            return cryptoki_sys::CKR_OPERATION_ACTIVE;
        }

        let key_id = match find_key_id(template) {
            Ok(key_id) => key_id,
            Err(err) => return err,
        };

        let handles = match self.find_key(key_id) {
            Ok(handles) => handles,
            Err(err) => return err,
        };

        self.enum_ctx = Some(EnumCtx { handles });

        cryptoki_sys::CKR_OK
    }
    fn find_key(&mut self, key_id: Option<String>) -> Result<Vec<CK_OBJECT_HANDLE>, CK_RV> {
        match key_id {
            Some(key_id) => {
                let (handle, _) = self.fetch_key(key_id)?;
                Ok(vec![handle])
            }
            None => self.fetch_all_keys(),
        }
    }

    fn fetch_all_keys(&mut self) -> Result<Vec<CK_OBJECT_HANDLE>, CK_RV> {
        if self.fetched_all_keys {
            return Ok(self
                .db
                .enumerate()
                .map(|(handle, _)| handle.into())
                .collect());
        }

        // clear the db to not have any double entries
        self.db.clear();

        let keys = default_api::keys_get(&self.slot.api_config, None).map_err(|err| {
            error!("Failed to fetch keys: {:?}", err);
            CKR_DEVICE_ERROR
        })?;

        let mut handles = Vec::new();

        for key in keys {
            let (handle, __library) = self.fetch_key(key.key)?;

            handles.push(handle);
        }
        Ok(handles)
    }

    fn fetch_key(&mut self, key_id: String) -> Result<(CK_OBJECT_HANDLE, Object), CK_RV> {
        let key_data =
            default_api::keys_key_id_get(&self.slot.api_config, &key_id).map_err(|err| {
                error!("Failed to fetch key {}: {:?}", key_id, err);
                CKR_DEVICE_ERROR
            })?;

        let object = db::object::Object::from_key_data(key_data, key_id);

        let handle = self.db.add_object(object.clone());

        Ok((handle, object))
    }
}

fn find_key_id(template: Option<CkRawAttrTemplate>) -> Result<Option<String>, CK_RV> {
    match template {
        Some(template) => {
            let mut key_id = None;
            for attr in template.iter() {
                if attr.type_() == CKA_ID {
                    key_id = Some(parse_str_from_attr(&attr)?);
                    break;
                }
                if attr.type_() == CKA_LABEL {
                    key_id = Some(parse_str_from_attr(&attr)?);
                }
            }
            Ok(key_id)
        }
        None => Ok(None),
    }
}

fn parse_str_from_attr(attr: &CkRawAttr) -> Result<String, CK_RV> {
    let bytes = attr.val_bytes().ok_or(CKR_ARGUMENTS_BAD)?;
    String::from_utf8(bytes.to_vec()).map_err(|_| CKR_ARGUMENTS_BAD)
}

#[derive(Clone, Debug)]
pub struct SignCtx {}
#[derive(Clone, Debug)]
pub struct EncryptCtx {}
#[derive(Clone, Debug)]
pub struct DecryptCtx {}

// context to find objects
#[derive(Clone, Debug)]
pub struct EnumCtx {
    pub handles: Vec<CK_SESSION_HANDLE>,
}
