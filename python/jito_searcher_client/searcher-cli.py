import time
from typing import List

from solana.rpc.commitment import Processed
from solders.keypair import Keypair
from solders.pubkey import Pubkey
from solders.system_program import transfer, TransferParams
from spl.memo.instructions import create_memo, MemoParams

from .generated.bundle_pb2 import Bundle
from .convert import tx_to_protobuf_packet
from .searcher import get_searcher_client
from .generated.searcher_pb2 import (
    ConnectedLeadersRequest,
    NextScheduledLeaderRequest,
    PendingTxSubscriptionRequest,
    NextScheduledLeaderResponse, SendBundleRequest,
)
from generated.searcher_pb2_grpc import SearcherServiceStub
import click

from solders.transaction import VersionedTransaction, Transaction
from solana.rpc.api import Client


@click.group("cli")
@click.pass_context
@click.option(
    "--keypair-path",
    help="Path to a keypair that is authenticated with the block engine.",
    required=True,
)
@click.option(
    "--block-engine-url",
    help="Block Engine URL",
    required=True,
)
def cli(
        ctx,
        keypair_path: str,
        block_engine_url: str,
):
    """
    This script can be used to interface with the block engine as a jito_searcher_client.
    """
    with open(keypair_path) as kp_path:
        kp = Keypair.from_json(kp_path.read())
    ctx.obj = get_searcher_client(block_engine_url, kp)


@click.command("mempool-accounts")
@click.pass_obj
@click.argument("accounts", required=True, nargs=-1)
def mempool_accounts(client: SearcherServiceStub, accounts: List[str]):
    """
    Stream pending transactions from write-locked accounts.
    """
    leader: NextScheduledLeaderResponse = client.GetNextScheduledLeader(
        NextScheduledLeaderRequest()
    )
    print(
        f"next scheduled leader is {leader.next_leader_identity} in {leader.next_leader_slot - leader.current_slot} slots"
    )

    for notification in client.SubscribePendingTransactions(
            PendingTxSubscriptionRequest(accounts=accounts)
    ):
        for packet in notification.transactions:
            print(VersionedTransaction.from_bytes(packet.data))


@click.command("next-scheduled-leader")
@click.pass_obj
def next_scheduled_leader(client: SearcherServiceStub):
    """
    Find information on the next scheduled leader.
    """
    next_leader = client.GetNextScheduledLeader(NextScheduledLeaderRequest())
    print(f"{next_leader=}")


@click.command("connected-leaders")
@click.pass_obj
def connected_leaders(client: SearcherServiceStub):
    """
    Get leaders connected to this block engine.
    """
    leaders = client.GetConnectedLeaders(ConnectedLeadersRequest())
    print(f"{leaders=}")


@click.command("tip-accounts")
@click.pass_obj
def tip_accounts(client: SearcherServiceStub):
    """
    Get the tip accounts from the block engine.
    """
    accounts = client.GetNextScheduledLeader(NextScheduledLeaderRequest())
    print(f"{accounts=}")


@click.command("send-bundle")
@click.pass_obj
@click.option(
    "--rpc-url",
    help="RPC URL path",
    type=str,
    required=True,
)
@click.option(
    "--payer",
    help="Path to payer keypair",
    type=str,
    required=True,
)
@click.option(
    "--message",
    help="Message in the bundle",
    type=str,
    required=True,
)
@click.option(
    "--num_txs",
    help="Number of transactions in the bundle (max is 5)",
    type=int,
    required=True,
)
@click.option(
    "--lamports",
    help="Number of lamports to tip in each transaction",
    type=int,
    required=True,
)
@click.option(
    "--tip_account",
    help="Tip account to tip",
    type=str,
    required=True,
)
def send_bundle(
        client: SearcherServiceStub,
        rpc_url: str,
        payer: str,
        message: str,
        num_txs: int,
        lamports: int,
        tip_account: str,
):
    """
    Send a bundle!
    """
    with open(payer) as kp_path:
        payer_kp = Keypair.from_json(kp_path.read())
    tip_account = Pubkey.from_string(tip_account)

    rpc_client = Client(rpc_url)
    balance = rpc_client.get_balance(payer_kp.pubkey()).value
    print(f"payer public key: {payer_kp.pubkey()} {balance=}")

    is_leader_slot = False
    print("waiting for jito leader...")
    while not is_leader_slot:
        time.sleep(0.5)
        next_leader: NextScheduledLeaderResponse = client.GetNextScheduledLeader(
            NextScheduledLeaderRequest()
        )
        num_slots_to_leader = next_leader.next_leader_slot - next_leader.current_slot
        print(f"waiting {num_slots_to_leader} slots to jito leader")
        is_leader_slot = num_slots_to_leader <= 2

    blockhash = rpc_client.get_latest_blockhash().value.blockhash
    block_height = rpc_client.get_block_height(Processed).value

    # Build bundle
    txs: List[Transaction] = []
    for idx in range(num_txs):
        transfer_ix = transfer(TransferParams(
            from_pubkey=payer_kp.pubkey(),
            to_pubkey=tip_account,
            lamports=lamports
        ))
        memp_ix = create_memo(MemoParams(program_id=Pubkey.from_string("MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr"),
                                         signer=payer_kp.pubkey(),
                                         message=bytes(f"jito bundle {idx}: {message}", "utf-8")))
        tx = Transaction.new_signed_with_payer(instructions=[memp_ix, transfer_ix],
                                               payer=payer_kp.pubkey(),
                                               signing_keypairs=[payer_kp],
                                               recent_blockhash=blockhash
                                               )
        print(f"{idx=} signature={tx.signatures[0]}")
        txs.append(tx)

    # Note: setting meta.size here is important so the block engine can deserialize the packet
    packets = [tx_to_protobuf_packet(tx) for tx in txs]

    uuid_response = client.SendBundle(SendBundleRequest(bundle=Bundle(header=None, packets=packets)))
    print(f"bundle uuid: {uuid_response.uuid}")

    for tx in txs:
        print(rpc_client.confirm_transaction(tx.signatures[0], Processed, sleep_seconds=0.5,
                                             last_valid_block_height=block_height + 10))


if __name__ == "__main__":
    cli.add_command(mempool_accounts)
    cli.add_command(next_scheduled_leader)
    cli.add_command(connected_leaders)
    cli.add_command(tip_accounts)
    cli.add_command(send_bundle)
    cli()
