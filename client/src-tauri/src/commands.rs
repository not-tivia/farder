use crate::state::AppState;
use farder_crypto::identity::Keypair;
use farder_node::node::PersonalNode;
use tauri::State;

#[tauri::command]
pub fn generate_keypair(state: State<AppState>) -> Result<String, String> {
    let keypair = Keypair::generate();
    let public_key = keypair.public_key().to_string();
    let node = PersonalNode::new_in_memory(keypair).map_err(|e| e.to_string())?;
    let mut node_lock = state.node.lock().map_err(|e| e.to_string())?;
    *node_lock = Some(node);
    Ok(public_key)
}

#[tauri::command]
pub fn get_public_key(state: State<AppState>) -> Result<String, String> {
    let node_lock = state.node.lock().map_err(|e| e.to_string())?;
    match &*node_lock { Some(node) => Ok(node.public_key().to_string()), None => Err("No identity yet".to_string()) }
}

#[tauri::command]
pub fn export_key(state: State<AppState>, passphrase: String) -> Result<Vec<u8>, String> {
    let node_lock = state.node.lock().map_err(|e| e.to_string())?;
    match &*node_lock { Some(node) => node.keypair.export_encrypted(&passphrase).map_err(|e| e.to_string()), None => Err("No identity yet".to_string()) }
}

#[tauri::command]
pub fn import_key(state: State<AppState>, data: Vec<u8>, passphrase: String) -> Result<String, String> {
    let keypair = Keypair::import_encrypted(&data, &passphrase).map_err(|e| e.to_string())?;
    let public_key = keypair.public_key().to_string();
    let node = PersonalNode::new_in_memory(keypair).map_err(|e| e.to_string())?;
    let mut node_lock = state.node.lock().map_err(|e| e.to_string())?;
    *node_lock = Some(node);
    Ok(public_key)
}
