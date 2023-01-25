import time
from typing import List

from solders.keypair import Keypair
from solders.rpc.responses import GetBalanceResp

from searcher import get_searcher_client
from generated.searcher_pb2 import (
    ConnectedLeadersRequest,
    NextScheduledLeaderRequest,
    PendingTxSubscriptionRequest,
    NextScheduledLeaderResponse,
)
from generated.searcher_pb2_grpc import SearcherServiceStub
import click

from solders.transaction import VersionedTransaction
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
    This script can be used to interface with the block engine as a searcher.
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


@click.command("connected-leaders-info")
@click.pass_obj
def connected_leaders_info(client: SearcherServiceStub):
    """
    Get connected leaders stake weight.
    """
    pass


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
        print(f"waiting {num_slots_to_leader} to jito leader")
        is_leader_slot = num_slots_to_leader <= 2

    blockhash = rpc_client.get_latest_blockhash().blockhash
    txs = []
    for idx in range(num_txs):
        txs.append(VersionedTransaction())


if __name__ == "__main__":
    cli.add_command(mempool_accounts)
    cli.add_command(next_scheduled_leader)
    cli.add_command(connected_leaders)
    cli.add_command(connected_leaders_info)
    cli.add_command(tip_accounts)
    cli.add_command(send_bundle)
    cli()
