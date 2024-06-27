use std::{
    collections::{HashMap, HashSet},
    net::{IpAddr, Ipv4Addr},
    time::Duration,
};

use bitcoin::{consensus::serialize, Amount, ScriptBuf};
use bitcoincore_rpc::{json::CreateRawTransactionInput, RpcApi};
use kyoto::{
    chain::checkpoints::HeaderCheckpoint,
    node::{client::Client, node::Node},
    TxBroadcast,
};

const RPC_USER: &str = "test";
const RPC_PASSWORD: &str = "kyoto";
const WALLET: &str = "test_kyoto";
const HOST: &str = "http://localhost:18443";
const PORT: u16 = 18444;

fn initialize_client() -> Result<bitcoincore_rpc::Client, bitcoincore_rpc::Error> {
    let rpc = bitcoincore_rpc::Client::new(
        HOST,
        bitcoincore_rpc::Auth::UserPass(RPC_USER.into(), RPC_PASSWORD.into()),
    )?;
    // Do a call that will only fail if we are not connected to RPC.
    let _ = rpc.get_best_block_hash()?;
    match rpc.load_wallet(WALLET) {
        Ok(_) => Ok(rpc),
        Err(_) => {
            rpc.create_wallet(WALLET, None, None, None, None)?;
            Ok(rpc)
        }
    }
}

async fn new_node(addrs: HashSet<ScriptBuf>) -> (Node, Client) {
    let host = (IpAddr::from(Ipv4Addr::new(0, 0, 0, 0)), PORT);
    let builder = kyoto::node::builder::NodeBuilder::new(bitcoin::Network::Regtest);
    let (node, client) = builder
        .add_peers(vec![host])
        .add_scripts(addrs)
        .build_with_databases((), ())
        .await;
    (node, client)
}

async fn new_node_sql(addrs: HashSet<ScriptBuf>) -> (Node, Client) {
    let host = (IpAddr::from(Ipv4Addr::new(0, 0, 0, 0)), PORT);
    let builder = kyoto::node::builder::NodeBuilder::new(bitcoin::Network::Regtest);
    let (node, client) = builder
        .add_peers(vec![host])
        .add_scripts(addrs)
        .build_node()
        .await;
    (node, client)
}

async fn new_node_anchor_sql(
    addrs: HashSet<ScriptBuf>,
    checkpoint: HeaderCheckpoint,
) -> (Node, Client) {
    let host = (IpAddr::from(Ipv4Addr::new(0, 0, 0, 0)), PORT);
    let builder = kyoto::node::builder::NodeBuilder::new(bitcoin::Network::Regtest);
    let (node, client) = builder
        .add_peers(vec![host])
        .add_scripts(addrs)
        .anchor_checkpoint(checkpoint)
        .build_node()
        .await;
    (node, client)
}

// Steps to run this test.
// 1. Run `scripts/regtest.sh`
// 2. Run `cargo test test_reorg -- --nocapture`
//
// If the test fails: run `scripts/kill.sh`
#[tokio::test]
async fn test_reorg() {
    let rpc_result = initialize_client();
    // If we can't fetch the genesis block then bitcoind is not running. Just exit.
    if let Err(_) = rpc_result {
        println!("Bitcoin Core is not running. Skipping this test...");
        return;
    }
    let rpc = rpc_result.unwrap();
    // Mine some blocks
    let miner = rpc.get_new_address(None, None).unwrap().assume_checked();
    rpc.generate_to_address(10, &miner).unwrap();
    tokio::time::sleep(Duration::from_secs(1)).await;
    let best = rpc.get_best_block_hash().unwrap();
    tokio::time::sleep(Duration::from_secs(1)).await;
    // Build and run a node
    let mut scripts = HashSet::new();
    let other = rpc.get_new_address(None, None).unwrap().assume_checked();
    scripts.insert(other.into());
    let (mut node, mut client) = new_node(scripts.clone()).await;
    tokio::task::spawn(async move { node.run().await });
    let (mut sender, mut recv) = client.split();
    while let Ok(message) = recv.recv().await {
        match message {
            kyoto::node::messages::NodeMessage::Dialog(d) => println!("{d}"),
            kyoto::node::messages::NodeMessage::Warning(e) => println!("{e}"),
            kyoto::node::messages::NodeMessage::Synced(update) => {
                assert_eq!(update.tip().hash, best);
                break;
            }
            _ => {}
        }
    }
    // Reorganize the blocks
    let old_best = best;
    let old_height = rpc.get_block_count().unwrap();
    rpc.invalidate_block(&best).unwrap();
    tokio::time::sleep(Duration::from_secs(2)).await;
    rpc.generate_to_address(2, &miner).unwrap();
    tokio::time::sleep(Duration::from_secs(2)).await;
    let best = rpc.get_best_block_hash().unwrap();
    // Make sure the reorg was caught
    while let Ok(message) = recv.recv().await {
        match message {
            kyoto::node::messages::NodeMessage::Dialog(d) => println!("{d}"),
            kyoto::node::messages::NodeMessage::Warning(e) => println!("{e}"),
            kyoto::node::messages::NodeMessage::BlocksDisconnected(blocks) => {
                assert_eq!(blocks.len(), 1);
                assert_eq!(blocks.first().unwrap().header.block_hash(), old_best);
                assert_eq!(old_height as u32, blocks.first().unwrap().height);
            }
            kyoto::node::messages::NodeMessage::Synced(update) => {
                assert_eq!(update.tip().hash, best);
                sender.shutdown().await.unwrap();
                break;
            }
            _ => {}
        }
    }
    client.shutdown().await.unwrap();
    rpc.stop().unwrap();
}

// Steps to run this test.
// 1. Delete `regtest` from `.bitcoin` or wherever your Bitcoin Core data is stored.
// 2. Run `scripts/regtest.sh`
// 3. Run `cargo test test_broadcast -- --nocapture`
//
// If the test fails: run `scripts/kill.sh`
#[tokio::test]
async fn test_broadcast() {
    let rpc_result = initialize_client();
    // If we can't fetch the genesis block then bitcoind is not running. Just exit.
    if let Err(_) = rpc_result {
        println!("Bitcoin Core is not running. Skipping this test...");
        return;
    }
    // Mine some blocks
    let rpc = rpc_result.unwrap();
    let miner = rpc.get_new_address(None, None).unwrap().assume_checked();
    rpc.generate_to_address(105, &miner).unwrap();
    tokio::time::sleep(Duration::from_secs(5)).await;
    // Send to a random address
    let other = rpc.get_new_address(None, None).unwrap().assume_checked();
    rpc.send_to_address(
        &other.clone(),
        Amount::from_btc(100.).unwrap(),
        None,
        None,
        Some(true),
        None,
        None,
        None,
    )
    .unwrap();
    // Confirm the transaction
    rpc.generate_to_address(10, &miner).unwrap();
    // Get inputs and outputs for a new transaction
    let inputs = rpc
        .list_unspent(Some(1), None, Some(&[&other]), None, None)
        .unwrap();
    let raw_tx_inputs: Vec<CreateRawTransactionInput> = inputs
        .iter()
        .map(|input| CreateRawTransactionInput {
            txid: input.txid,
            vout: input.vout,
            sequence: None,
        })
        .collect::<_>();
    let burn = rpc.get_new_address(None, None).unwrap().assume_checked();
    let mut outs = HashMap::new();
    outs.insert(burn.to_string(), Amount::from_sat(1000));
    // Create and sign the transaction
    let tx = rpc
        .create_raw_transaction(&raw_tx_inputs, &outs, None, None)
        .unwrap();
    let signed = rpc
        .sign_raw_transaction_with_wallet(&serialize(&tx), None, None)
        .unwrap();
    // Make sure we sync up to the tip under usual conditions.
    let mut scripts = HashSet::new();
    scripts.insert(burn.into());
    let (mut node, mut client) = new_node(scripts.clone()).await;
    tokio::task::spawn(async move { node.run().await });
    let (mut sender, mut recv) = client.split();
    // Broadcast the transaction to the network
    sender
        .broadcast_tx(TxBroadcast::new(
            signed.transaction().unwrap(),
            kyoto::TxBroadcastPolicy::AllPeers,
        ))
        .await
        .unwrap();
    while let Ok(message) = recv.recv().await {
        match message {
            kyoto::node::messages::NodeMessage::Dialog(d) => println!("{d}"),
            kyoto::node::messages::NodeMessage::Warning(e) => println!("{e}"),
            kyoto::node::messages::NodeMessage::TxSent(t) => {
                println!("Sent transaction: {t}");
                break;
            }
            kyoto::node::messages::NodeMessage::TxBroadcastFailure(_) => {
                panic!()
            }
            _ => {}
        }
    }
    client.shutdown().await.unwrap();
    rpc.stop().unwrap();
}

// Steps to run this test.
// 1. Run `scripts/regtest.sh`
// 2. Run `cargo test test_long_chain -- --nocapture`
//
// If the test fails: run `scripts/kill.sh`
#[tokio::test]
async fn test_long_chain() {
    let rpc_result = initialize_client();
    // If we can't fetch the genesis block then bitcoind is not running. Just exit.
    if let Err(_) = rpc_result {
        println!("Bitcoin Core is not running. Skipping this test...");
        return;
    }
    let rpc = rpc_result.unwrap();
    // Mine a lot of blocks
    let miner = rpc.get_new_address(None, None).unwrap().assume_checked();
    rpc.generate_to_address(500, &miner).unwrap();
    tokio::time::sleep(Duration::from_secs(15)).await;
    let best = rpc.get_best_block_hash().unwrap();
    let mut scripts = HashSet::new();
    let other = rpc.get_new_address(None, None).unwrap().assume_checked();
    scripts.insert(other.into());
    let (mut node, mut client) = new_node(scripts.clone()).await;
    tokio::task::spawn(async move { node.run().await });
    let (mut sender, mut recv) = client.split();
    while let Ok(message) = recv.recv().await {
        match message {
            kyoto::node::messages::NodeMessage::Dialog(d) => println!("{d}"),
            kyoto::node::messages::NodeMessage::Warning(e) => println!("{e}"),
            kyoto::node::messages::NodeMessage::Synced(update) => {
                assert_eq!(update.tip().hash, best);
                break;
            }
            _ => {}
        }
    }
    sender.shutdown().await.unwrap();
    rpc.stop().unwrap();
}

// Steps to run this test.
// 1. Delete `regtest` from `.bitcoin` or wherever your Bitcoin Core data is stored.
// 2. Delete `data` from your root directory, if it exists.
// 3. Run `scripts/regtest.sh`
// 4. Run `cargo test test_sql_reorg -- --nocapture`
//
// If the test fails: run `scripts/kill.sh`
#[tokio::test]
async fn test_sql_reorg() {
    let rpc_result = initialize_client();
    // If we can't fetch the genesis block then bitcoind is not running. Just exit.
    if let Err(_) = rpc_result {
        println!("Bitcoin Core is not running. Skipping this test...");
        return;
    }
    let rpc = rpc_result.unwrap();
    // Mine some blocks.
    let miner = rpc.get_new_address(None, None).unwrap().assume_checked();
    rpc.generate_to_address(10, &miner).unwrap();
    tokio::time::sleep(Duration::from_secs(1)).await;
    let best = rpc.get_best_block_hash().unwrap();
    tokio::time::sleep(Duration::from_secs(1)).await;
    let mut scripts = HashSet::new();
    let other = rpc.get_new_address(None, None).unwrap().assume_checked();
    scripts.insert(other.into());
    let (mut node, mut client) = new_node_sql(scripts.clone()).await;
    tokio::task::spawn(async move { node.run().await });
    let (_, mut recv) = client.split();
    while let Ok(message) = recv.recv().await {
        match message {
            kyoto::node::messages::NodeMessage::Dialog(d) => println!("{d}"),
            kyoto::node::messages::NodeMessage::Warning(e) => println!("{e}"),
            kyoto::node::messages::NodeMessage::Synced(update) => {
                println!("Done");
                assert_eq!(update.tip().hash, best);
                break;
            }
            _ => {}
        }
    }
    client.shutdown().await.unwrap();
    // Reorganize the blocks
    let old_best = best;
    let old_height = rpc.get_block_count().unwrap();
    rpc.invalidate_block(&best).unwrap();
    tokio::time::sleep(Duration::from_secs(2)).await;
    rpc.generate_to_address(2, &miner).unwrap();
    tokio::time::sleep(Duration::from_secs(2)).await;
    let best = rpc.get_best_block_hash().unwrap();
    // Spin up the node on a cold start
    let (mut node, mut client) = new_node_sql(scripts.clone()).await;
    tokio::task::spawn(async move { node.run().await });
    let (_, mut recv) = client.split();
    // Make sure the reorganization is caught after a cold start
    while let Ok(message) = recv.recv().await {
        match message {
            kyoto::node::messages::NodeMessage::Dialog(d) => println!("{d}"),
            kyoto::node::messages::NodeMessage::Warning(e) => println!("{e}"),
            kyoto::node::messages::NodeMessage::BlocksDisconnected(blocks) => {
                assert_eq!(blocks.len(), 1);
                assert_eq!(blocks.first().unwrap().header.block_hash(), old_best);
                assert_eq!(old_height as u32, blocks.first().unwrap().height);
            }
            kyoto::node::messages::NodeMessage::Synced(update) => {
                println!("Done");
                assert_eq!(update.tip().hash, best);
                break;
            }
            _ => {}
        }
    }
    client.shutdown().await.unwrap();
    // Mine more blocks
    rpc.generate_to_address(2, &miner).unwrap();
    tokio::time::sleep(Duration::from_secs(2)).await;
    let best = rpc.get_best_block_hash().unwrap();
    // Make sure the node does not have any corrupted headers
    let (mut node, mut client) = new_node_sql(scripts.clone()).await;
    tokio::task::spawn(async move { node.run().await });
    let (_, mut recv) = client.split();
    // The node properly syncs after persisting a reorg
    while let Ok(message) = recv.recv().await {
        match message {
            kyoto::node::messages::NodeMessage::Dialog(d) => println!("{d}"),
            kyoto::node::messages::NodeMessage::Warning(e) => println!("{e}"),
            kyoto::node::messages::NodeMessage::Synced(update) => {
                println!("Done");
                assert_eq!(update.tip().hash, best);
                break;
            }
            _ => {}
        }
    }
    client.shutdown().await.unwrap();
    rpc.stop().unwrap();
}

// Steps to run this test.
// 1. Delete `regtest` from `.bitcoin` or wherever your Bitcoin Core data is stored.
// 2. Delete `data` from your root directory, if it exists.
// 3. Run `scripts/regtest.sh`
// 4. Run `cargo test test_two_deep_reorg -- --nocapture`
//
// If the test fails: run `scripts/kill.sh`
#[tokio::test]
async fn test_two_deep_reorg() {
    let rpc_result = initialize_client();
    // If we can't fetch the genesis block then bitcoind is not running. Just exit.
    if let Err(_) = rpc_result {
        println!("Bitcoin Core is not running. Skipping this test...");
        return;
    }
    let rpc = rpc_result.unwrap();
    // Mine some blocks.
    let miner = rpc.get_new_address(None, None).unwrap().assume_checked();
    rpc.generate_to_address(10, &miner).unwrap();
    tokio::time::sleep(Duration::from_secs(1)).await;
    let best = rpc.get_best_block_hash().unwrap();
    tokio::time::sleep(Duration::from_secs(1)).await;
    let mut scripts = HashSet::new();
    let other = rpc.get_new_address(None, None).unwrap().assume_checked();
    scripts.insert(other.into());
    let (mut node, mut client) = new_node_sql(scripts.clone()).await;
    tokio::task::spawn(async move { node.run().await });
    let (_, mut recv) = client.split();
    while let Ok(message) = recv.recv().await {
        match message {
            kyoto::node::messages::NodeMessage::Dialog(d) => println!("{d}"),
            kyoto::node::messages::NodeMessage::Warning(e) => println!("{e}"),
            kyoto::node::messages::NodeMessage::Synced(update) => {
                println!("Done");
                assert_eq!(update.tip().hash, best);
                break;
            }
            _ => {}
        }
    }
    client.shutdown().await.unwrap();
    // Reorganize the blocks
    let old_height = rpc.get_block_count().unwrap();
    let old_best = best;
    rpc.invalidate_block(&best).unwrap();
    tokio::time::sleep(Duration::from_secs(2)).await;
    let best = rpc.get_best_block_hash().unwrap();
    rpc.invalidate_block(&best).unwrap();
    tokio::time::sleep(Duration::from_secs(2)).await;
    rpc.generate_to_address(3, &miner).unwrap();
    tokio::time::sleep(Duration::from_secs(2)).await;
    let best = rpc.get_best_block_hash().unwrap();
    // Make sure the reorganization is caught after a cold start
    let (mut node, mut client) = new_node_sql(scripts.clone()).await;
    tokio::task::spawn(async move { node.run().await });
    let (_, mut recv) = client.split();
    while let Ok(message) = recv.recv().await {
        match message {
            kyoto::node::messages::NodeMessage::Dialog(d) => println!("{d}"),
            kyoto::node::messages::NodeMessage::Warning(e) => println!("{e}"),
            kyoto::node::messages::NodeMessage::BlocksDisconnected(blocks) => {
                assert_eq!(blocks.len(), 2);
                assert_eq!(blocks.last().unwrap().header.block_hash(), old_best);
                assert_eq!(old_height as u32, blocks.last().unwrap().height);
            }
            kyoto::node::messages::NodeMessage::Synced(update) => {
                println!("Done");
                assert_eq!(update.tip().hash, best);
                break;
            }
            _ => {}
        }
    }
    client.shutdown().await.unwrap();
    // Mine more blocks
    rpc.generate_to_address(2, &miner).unwrap();
    tokio::time::sleep(Duration::from_secs(2)).await;
    let best = rpc.get_best_block_hash().unwrap();
    // Make sure the node does not have any corrupted headers
    let (mut node, mut client) = new_node_sql(scripts.clone()).await;
    tokio::task::spawn(async move { node.run().await });
    let (_, mut recv) = client.split();
    // The node properly syncs after persisting a reorg
    while let Ok(message) = recv.recv().await {
        match message {
            kyoto::node::messages::NodeMessage::Dialog(d) => println!("{d}"),
            kyoto::node::messages::NodeMessage::Warning(e) => println!("{e}"),
            kyoto::node::messages::NodeMessage::Synced(update) => {
                println!("Done");
                assert_eq!(update.tip().hash, best);
                break;
            }
            _ => {}
        }
    }
    client.shutdown().await.unwrap();
    rpc.stop().unwrap();
}

// Steps to run this test.
// 1. Delete `regtest` from `.bitcoin` or wherever your Bitcoin Core data is stored.
// 2. Delete `data` from your root directory, if it exists.
// 3. Run `scripts/regtest.sh`
// 4. Run `scripts/mine.sh`
// 5. Run `cargo test test_sql_stale_anchor -- --nocapture`
//
// If the test fails: run `scripts/kill.sh`
#[tokio::test]
async fn test_sql_stale_anchor() {
    let rpc = bitcoincore_rpc::Client::new(
        HOST,
        bitcoincore_rpc::Auth::UserPass(RPC_USER.into(), RPC_PASSWORD.into()),
    )
    .unwrap();
    // Do a call that will only fail if we are not connected to RPC.
    if let Err(_) = rpc.get_new_address(None, None) {
        println!("There is no wallet loaded. Have you ran `mine.sh`?");
        return;
    }
    // Get an address and the tip of the chain.
    let miner = rpc.get_new_address(None, None).unwrap().assume_checked();
    let best = rpc.get_best_block_hash().unwrap();
    let mut scripts = HashSet::new();
    let other = rpc.get_new_address(None, None).unwrap().assume_checked();
    scripts.insert(other.into());
    let (mut node, mut client) = new_node_sql(scripts.clone()).await;
    tokio::task::spawn(async move { node.run().await });
    let (_, mut recv) = client.split();
    while let Ok(message) = recv.recv().await {
        match message {
            kyoto::node::messages::NodeMessage::Dialog(d) => println!("{d}"),
            kyoto::node::messages::NodeMessage::Warning(e) => println!("{e}"),
            kyoto::node::messages::NodeMessage::Synced(update) => {
                println!("Done");
                assert_eq!(update.tip().hash, best);
                break;
            }
            _ => {}
        }
    }
    client.shutdown().await.unwrap();
    // Reorganize the blocks
    let old_best = best;
    let old_height = rpc.get_block_count().unwrap();
    rpc.invalidate_block(&best).unwrap();
    tokio::time::sleep(Duration::from_secs(2)).await;
    rpc.generate_to_address(2, &miner).unwrap();
    tokio::time::sleep(Duration::from_secs(2)).await;
    let best = rpc.get_best_block_hash().unwrap();
    // Spin up the node on a cold start with a stale tip
    let (mut node, mut client) = new_node_anchor_sql(
        scripts.clone(),
        HeaderCheckpoint::new(old_height as u32, old_best),
    )
    .await;
    tokio::task::spawn(async move { node.run().await });
    let (_, mut recv) = client.split();
    // Ensure SQL is able to catch the fork by loading in headers from the database
    while let Ok(message) = recv.recv().await {
        match message {
            kyoto::node::messages::NodeMessage::Dialog(d) => println!("{d}"),
            kyoto::node::messages::NodeMessage::Warning(e) => println!("{e}"),
            kyoto::node::messages::NodeMessage::BlocksDisconnected(blocks) => {
                assert_eq!(blocks.len(), 1);
                assert_eq!(blocks.first().unwrap().header.block_hash(), old_best);
                assert_eq!(old_height as u32, blocks.first().unwrap().height);
            }
            kyoto::node::messages::NodeMessage::Synced(update) => {
                println!("Done");
                assert_eq!(update.tip().hash, best);
                break;
            }
            _ => {}
        }
    }
    client.shutdown().await.unwrap();
    // Don't do anything, but reload the node from the checkpoint
    let cp = rpc.get_best_block_hash().unwrap();
    let old_height = rpc.get_block_count().unwrap();
    let best = rpc.get_best_block_hash().unwrap();
    // Make sure the node does not have any corrupted headers
    let (mut node, mut client) = new_node_anchor_sql(
        scripts.clone(),
        HeaderCheckpoint::new(old_height as u32, cp),
    )
    .await;
    tokio::task::spawn(async move { node.run().await });
    let (_, mut recv) = client.split();
    // The node properly syncs after persisting a reorg
    while let Ok(message) = recv.recv().await {
        match message {
            kyoto::node::messages::NodeMessage::Dialog(d) => println!("{d}"),
            kyoto::node::messages::NodeMessage::Warning(e) => println!("{e}"),
            kyoto::node::messages::NodeMessage::Synced(update) => {
                println!("Done");
                assert_eq!(update.tip().hash, best);
                break;
            }
            _ => {}
        }
    }
    client.shutdown().await.unwrap();
    // Mine more blocks and reload from the checkpoint
    let cp = rpc.get_best_block_hash().unwrap();
    let old_height = rpc.get_block_count().unwrap();
    rpc.generate_to_address(2, &miner).unwrap();
    tokio::time::sleep(Duration::from_secs(2)).await;
    let best = rpc.get_best_block_hash().unwrap();
    // Make sure the node does not have any corrupted headers
    let (mut node, mut client) = new_node_anchor_sql(
        scripts.clone(),
        HeaderCheckpoint::new(old_height as u32, cp),
    )
    .await;
    tokio::task::spawn(async move { node.run().await });
    let (_, mut recv) = client.split();
    // The node properly syncs after persisting a reorg
    while let Ok(message) = recv.recv().await {
        match message {
            kyoto::node::messages::NodeMessage::Dialog(d) => println!("{d}"),
            kyoto::node::messages::NodeMessage::Warning(e) => println!("{e}"),
            kyoto::node::messages::NodeMessage::Synced(update) => {
                println!("Done");
                assert_eq!(update.tip().hash, best);
                break;
            }
            _ => {}
        }
    }
    client.shutdown().await.unwrap();
    rpc.stop().unwrap();
}
