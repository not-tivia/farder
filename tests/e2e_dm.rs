use farder_crypto::identity::Keypair;
use farder_node::node::PersonalNode;
use farder_protocol::{codec, messages::Message};

#[test]
fn test_e2e_dm_two_users() {
    // Setup: Two users
    let mut alice = PersonalNode::new_in_memory(Keypair::generate()).unwrap();
    let mut bob = PersonalNode::new_in_memory(Keypair::generate()).unwrap();

    // Key exchange
    let alice_session = alice.session_public_key_bytes();
    let bob_session = bob.session_public_key_bytes();
    alice.complete_key_exchange(&bob.public_key(), &bob_session);
    bob.complete_key_exchange(&alice.public_key(), &alice_session);

    // Alice sends message
    let original_text = b"Hey Bob, this is a private message through Farder!";
    let encrypted_msg = alice.prepare_dm(&bob.public_key(), original_text).unwrap();

    // Simulate wire transport
    let wire_bytes = codec::encode(&encrypted_msg).unwrap();
    let received_msg: Message = codec::decode(&wire_bytes).unwrap();

    // Bob decrypts
    match received_msg {
        Message::EncryptedDm { sender, ciphertext, timestamp } => {
            assert_eq!(sender, alice.public_key());
            let decrypted = bob.receive_dm(&sender, &ciphertext, timestamp).unwrap();
            assert_eq!(decrypted, original_text);
        }
        _ => panic!("Expected EncryptedDm"),
    }

    // Bob replies
    let reply_text = b"Got it Alice! Farder works!";
    let reply_msg = bob.prepare_dm(&alice.public_key(), reply_text).unwrap();
    let reply_bytes = codec::encode(&reply_msg).unwrap();
    let received_reply: Message = codec::decode(&reply_bytes).unwrap();

    match received_reply {
        Message::EncryptedDm { sender, ciphertext, timestamp } => {
            assert_eq!(sender, bob.public_key());
            let decrypted = alice.receive_dm(&sender, &ciphertext, timestamp).unwrap();
            assert_eq!(decrypted, reply_text);
        }
        _ => panic!("Expected EncryptedDm"),
    }

    // Verify message history
    let alice_history = alice.message_store.get_messages(&bob.public_key(), 50, 0).unwrap();
    assert_eq!(alice_history.len(), 2);
    assert!(alice_history[0].is_outgoing);
    assert!(!alice_history[1].is_outgoing);

    let bob_history = bob.message_store.get_messages(&alice.public_key(), 50, 0).unwrap();
    assert_eq!(bob_history.len(), 2);
    assert!(!bob_history[0].is_outgoing);
    assert!(bob_history[1].is_outgoing);
}

#[test]
fn test_e2e_third_party_cannot_decrypt() {
    let mut alice = PersonalNode::new_in_memory(Keypair::generate()).unwrap();
    let mut bob = PersonalNode::new_in_memory(Keypair::generate()).unwrap();
    let mut eve = PersonalNode::new_in_memory(Keypair::generate()).unwrap();

    // Alice and Bob exchange keys
    let alice_session = alice.session_public_key_bytes();
    let bob_session = bob.session_public_key_bytes();
    alice.complete_key_exchange(&bob.public_key(), &bob_session);
    bob.complete_key_exchange(&alice.public_key(), &alice_session);

    // Eve exchanges keys with Alice (different session)
    let _eve_session = eve.session_public_key_bytes();
    let alice_session2 = alice.session_public_key_bytes();
    eve.complete_key_exchange(&alice.public_key(), &alice_session2);

    // Alice sends to Bob
    let msg = alice.prepare_dm(&bob.public_key(), b"Secret!").unwrap();

    match msg {
        Message::EncryptedDm { sender, ciphertext, timestamp } => {
            // Bob can decrypt
            assert!(bob.receive_dm(&sender, &ciphertext, timestamp).is_ok());
            // Eve cannot
            assert!(eve.receive_dm(&sender, &ciphertext, timestamp).is_err());
        }
        _ => panic!("Expected EncryptedDm"),
    }
}
