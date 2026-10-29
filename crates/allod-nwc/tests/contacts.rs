//! Contacts from an in-process relay: follow list → profiles → pay targets.

use std::time::Duration;

use allod_nwc::contacts::Contacts;
use nostr::prelude::*;
use nostr_relay_builder::MockRelay;
use nostr_sdk::Client;

#[tokio::test]
async fn follow_list_profiles_and_pay_targets() {
    let relay = MockRelay::run().await.unwrap();
    let url = relay.url().await.to_string();
    let owner = Keys::generate();
    let (bob, carol, dave) = (Keys::generate(), Keys::generate(), Keys::generate());
    let pub_client = Client::builder().build();
    pub_client.add_relay(url.as_str()).await.unwrap();
    pub_client.connect().await;
    let send = |e: Event| {
        let c = pub_client.clone();
        async move { c.send_event(&e).await.unwrap() }
    };
    let old = Timestamp::from(Timestamp::now().as_secs() - 100);
    // An older follow list that must be ignored, then the current one.
    send(
        EventBuilder::new(Kind::ContactList, "")
            .tags([Tag::public_key(dave.public_key())])
            .custom_created_at(old)
            .sign_with_keys(&owner)
            .unwrap(),
    )
    .await;
    send(
        EventBuilder::new(Kind::ContactList, "")
            .tags([
                Tag::public_key(bob.public_key()),
                Tag::public_key(carol.public_key()),
                Tag::public_key(dave.public_key()),
            ])
            .sign_with_keys(&owner)
            .unwrap(),
    )
    .await;
    send(
        EventBuilder::new(
            Kind::Metadata,
            r#"{"name":"bob","lud16":"bob@example.com","lno":"lno1bobsoffer"}"#,
        )
        .sign_with_keys(&bob)
        .unwrap(),
    )
    .await;
    send(
        EventBuilder::new(Kind::Metadata, r#"{"name":"carol-old"}"#)
            .custom_created_at(old)
            .sign_with_keys(&carol)
            .unwrap(),
    )
    .await;
    send(
        EventBuilder::new(
            Kind::Metadata,
            r#"{"display_name":"Carol","lud16":"carol@example.com"}"#,
        )
        .sign_with_keys(&carol)
        .unwrap(),
    )
    .await;
    // dave has no kind 0, but publishes a NIP-B1 offer list (kind 10058).
    send(
        EventBuilder::new(Kind::Custom(10058), "")
            .tags([Tag::parse(["offer", "lno1davesoffer"]).unwrap()])
            .sign_with_keys(&dave)
            .unwrap(),
    )
    .await;
    // carol also has a NIP-A3 payto list with a BOLT12 offer in a bip321 URI.
    send(
        EventBuilder::new(Kind::Custom(10133), "")
            .tags([Tag::parse(["payto", "bip321", "bitcoin:?lno=lno1carolsoffer"]).unwrap()])
            .sign_with_keys(&carol)
            .unwrap(),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;

    let contacts = std::sync::Arc::new(Contacts::new(vec![url]));
    let list = contacts.contacts(&owner.public_key().to_hex(), false).await.unwrap();
    assert_eq!(list.len(), 3, "{list:?}");
    let by = |pk: &PublicKey| list.iter().find(|p| p.pubkey == pk.to_hex()).unwrap().clone();
    assert_eq!(by(&bob.public_key()).pay_target.as_deref(), Some("lno1bobsoffer"));
    let c = by(&carol.public_key());
    assert_eq!(
        (c.name.as_deref(), c.pay_target.as_deref()),
        (Some("Carol"), Some("lno1carolsoffer"))
    );
    assert_eq!(
        c.targets.iter().map(|t| t.kind).collect::<Vec<_>>(),
        ["bolt12", "lightning_address"]
    );
    assert_eq!(
        by(&dave.public_key()).pay_target.as_deref(),
        Some("lno1davesoffer"),
        "NIP-B1 offer without a kind 0"
    );
    // Profile lookup for a zapper's pubkey uses the same cache.
    let p = contacts.profiles(&[bob.public_key().to_hex()], false).await;
    assert_eq!(p[0].name.as_deref(), Some("bob"));
}

/// NIP-65 outbox: a contact whose profile is only on their own relay, and an owner whose
/// follow list is only on their own relay, are found through their kind 10002 lists on
/// the index relay.
#[tokio::test]
async fn outbox_finds_profiles_on_their_own_relays() {
    let index = MockRelay::run().await.unwrap();
    let own = MockRelay::run().await.unwrap();
    let (index_url, own_url) = (index.url().await.to_string(), own.url().await.to_string());
    // In-process relays listen on ws://127.0.0.1, normally skipped as private.
    let owner = Keys::generate();
    let erin = Keys::generate();
    let publish = |url: String, e: Event| async move {
        let c = Client::builder().build();
        c.add_relay(url.as_str()).await.unwrap();
        c.connect().await;
        c.send_event(&e).await.unwrap();
    };
    // Erin's profile exists only on her own relay.
    publish(
        own_url.clone(),
        EventBuilder::new(Kind::Metadata, r#"{"name":"Erin","lud16":"erin@pay.example"}"#)
            .sign_with_keys(&erin)
            .unwrap(),
    )
    .await;
    // The owner's follow list exists only on the owner's own relay.
    publish(
        own_url.clone(),
        EventBuilder::new(Kind::ContactList, "")
            .tags([Tag::public_key(erin.public_key())])
            .sign_with_keys(&owner)
            .unwrap(),
    )
    .await;
    // Both publish relay lists on the index relay pointing at the own relay.
    for k in [&owner, &erin] {
        publish(
            index_url.clone(),
            EventBuilder::new(Kind::RelayList, "")
                .tags([Tag::parse(["r", own_url.as_str()]).unwrap()])
                .sign_with_keys(k)
                .unwrap(),
        )
        .await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;

    let contacts = std::sync::Arc::new(Contacts::new(vec![index_url]).allow_private_relays());
    let owner_hex = owner.public_key().to_hex();
    let list = contacts.contacts(&owner_hex, false).await.unwrap();
    assert_eq!(list.len(), 1, "follow list found on the owner's own relay: {list:?}");
    // The index relay has no profile for Erin: the first answer comes back at once, and
    // the lookup on her own relay runs in the background.
    assert!(list[0].pay_target.is_none());
    let start = std::time::Instant::now();
    let erin = loop {
        let list = contacts.contacts(&owner_hex, false).await.unwrap();
        if list[0].pay_target.is_some() {
            break list[0].clone();
        }
        assert!(start.elapsed() < Duration::from_secs(20), "outbox lookup did not finish");
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    assert_eq!(erin.name.as_deref(), Some("Erin"));
    assert_eq!(
        erin.pay_target.as_deref(),
        Some("erin@pay.example"),
        "profile found on Erin's relay"
    );
    assert!(!contacts.discovering());
}

#[tokio::test]
async fn outbox_finds_offer_even_when_index_has_lightning_address() {
    let index = MockRelay::run().await.unwrap();
    let own = MockRelay::run().await.unwrap();
    let (index_url, own_url) = (index.url().await.to_string(), own.url().await.to_string());
    let owner = Keys::generate();
    let payee = Keys::generate();
    let publish = |url: String, e: Event| async move {
        let client = Client::builder().build();
        client.add_relay(url.as_str()).await.unwrap();
        client.connect().await;
        client.send_event(&e).await.unwrap();
    };
    publish(
        index_url.clone(),
        EventBuilder::new(Kind::ContactList, "")
            .tags([Tag::public_key(payee.public_key())])
            .sign_with_keys(&owner)
            .unwrap(),
    )
    .await;
    publish(
        index_url.clone(),
        EventBuilder::new(Kind::Metadata, r#"{"name":"Payee","lud16":"payee@example.com"}"#)
            .custom_created_at(Timestamp::from(Timestamp::now().as_secs() - 100))
            .sign_with_keys(&payee)
            .unwrap(),
    )
    .await;
    publish(
        index_url.clone(),
        EventBuilder::new(Kind::RelayList, "")
            .tags([Tag::parse(["r", own_url.as_str()]).unwrap()])
            .sign_with_keys(&payee)
            .unwrap(),
    )
    .await;
    publish(
        own_url,
        EventBuilder::new(
            Kind::Metadata,
            r#"{"name":"Payee","lud16":"payee@example.com","lno":"lno1payeestoffer"}"#,
        )
        .sign_with_keys(&payee)
        .unwrap(),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;

    let contacts = std::sync::Arc::new(Contacts::new(vec![index_url]).allow_private_relays());
    let owner_hex = owner.public_key().to_hex();
    let first = contacts.contacts(&owner_hex, false).await.unwrap();
    assert_eq!(first[0].pay_target.as_deref(), Some("payee@example.com"));
    let start = std::time::Instant::now();
    loop {
        let list = contacts.contacts(&owner_hex, false).await.unwrap();
        if list[0].offer.as_deref() == Some("lno1payeestoffer") {
            assert_eq!(list[0].pay_target.as_deref(), Some("lno1payeestoffer"));
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(20), "BOLT12 offer not found");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}
