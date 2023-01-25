import sys
from dataclasses import dataclass
from typing import List

from solders.keypair import Keypair
from searcher import get_searcher_client
from generated.searcher_pb2 import (
    GetTipAccountsRequest,
    ConnectedLeadersRequest,
    NextScheduledLeaderRequest,
    PendingTxSubscriptionRequest,
    PendingTxNotification,
)
from generated.searcher_pb2_grpc import SearcherServiceStub
import click

from solders.pubkey import Pubkey
from solders.transaction import VersionedTransaction


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
def send_bundle(client: SearcherServiceStub):
    """
    Send a bundle!
    """
    pass


if __name__ == "__main__":
    cli.add_command(mempool_accounts)
    cli.add_command(next_scheduled_leader)
    cli.add_command(connected_leaders)
    cli.add_command(connected_leaders_info)
    cli.add_command(tip_accounts)
    cli.add_command(send_bundle)
    cli()
