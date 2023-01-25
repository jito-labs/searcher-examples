from dataclasses import dataclass

from solders.keypair import Keypair
from searcher import get_searcher_client
from generated.searcher_pb2 import GetTipAccountsRequest
import click


# rpc SubscribeBundleResults (SubscribeBundleResultsRequest) returns (stream bundle.BundleResult) {}
#
#   // RPC endpoint to subscribe to pending transactions. Clients can provide a list of base58 encoded accounts.
#   // Any transactions that write-lock the provided accounts will be streamed to the searcher.
#   rpc SubscribePendingTransactions (PendingTxSubscriptionRequest) returns (stream PendingTxNotification) {}
#
#   rpc SendBundle (SendBundleRequest) returns (SendBundleResponse) {}
#
#   // Returns the next scheduled leader connected to the block engine.
#   rpc GetNextScheduledLeader (NextScheduledLeaderRequest) returns (NextScheduledLeaderResponse) {}
#
#   // Returns information on connected leader slots
#   rpc GetConnectedLeaders (ConnectedLeadersRequest) returns (ConnectedLeadersResponse) {}
#
#   // Returns the tip accounts searchers shall transfer funds to for the leader to claim.
#   rpc GetTipAccounts (GetTipAccountsRequest) returns (GetTipAccountsResponse) {}


class Context:
    keypair: Keypair


@click.group("cli")
@click.pass_context
@click.argument("keypair_path")
@click.argument("block_engine_url")
def cli(
    ctx: Context,
    keypair,
):
    pass


@click.command("mempool-accounts")
def mempool_accounts():
    print("mempool_accounts")
    pass


@click.command("next-scheduled-leader")
def next_scheduled_leader():
    print("bar")
    pass


@click.command("connected-leaders")
def connected_leaders():
    print("bar")
    pass


@click.command("next-scheduled-leader")
def next_scheduled_leader():
    print("bar")
    pass


@click.command("tip-accounts")
def tip_accounts():
    print("bar")
    pass


@click.command("send-bundle")
def send_bundle():
    print("bar")
    pass


# def main():
#     keypair = Keypair()
#     searcher = get_searcher_client("frankfurt.mainnet.block-engine.jito.wtf", keypair)
#     tip_accounts = searcher.GetTipAccounts(GetTipAccountsRequest())


if __name__ == "__main__":
    cli.add_command(mempool_accounts)
    cli.add_command(next_scheduled_leader)
    cli.add_command(mempool_accounts)
    cli.add_command(mempool_accounts)
    cli.add_command(mempool_accounts)
    cli.add_command(mempool_accounts)
    cli.add_command(mempool_accounts)
    cli()
