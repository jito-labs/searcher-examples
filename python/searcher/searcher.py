import time
from dataclasses import dataclass
from typing import Optional, List, Tuple

from grpc.aio import ClientCallDetails

from generated.searcher_pb2_grpc import SearcherServiceStub
from generated.searcher_pb2 import GetTipAccountsRequest
from generated.auth_pb2_grpc import AuthServiceStub
from generated.auth_pb2 import (
    GenerateAuthChallengeRequest,
    Role,
    GenerateAuthTokensRequest,
    GenerateAuthTokensResponse,
)
from grpc import (
    intercept_channel,
    ssl_channel_credentials,
    secure_channel,
    UnaryUnaryClientInterceptor,
    UnaryStreamClientInterceptor,
    StreamUnaryClientInterceptor,
    StreamStreamClientInterceptor,
)

from solders.keypair import Keypair


@dataclass
class JwtToken:
    # jwt token string
    token: str
    # time in seconds since epoch when the token expires
    expiration: int


class SearcherInterceptor(
    UnaryUnaryClientInterceptor,
    UnaryStreamClientInterceptor,
    StreamUnaryClientInterceptor,
    StreamStreamClientInterceptor,
):
    """
    The searcher interceptor is responsible for authenticating with the block engine.
    Authentication happens in a challenge-response handshake.
    1. Request a challenge and provide your public key.
    2. Get challenge and sign a message "{pubkey}-{challenge}".
    3. Get back a refresh token and access token.

    When the access token expires, use the refresh token to get a new one.
    When the refresh token expires, perform the challenge-response handshake again.
    """

    def __init__(self, url: str, kp: Keypair):
        """

        :param url: url of the Block Engine without http or https.
        :param kp: block engine authentication keypair
        """
        self._url = url
        self._kp = kp

        self._access_token: Optional[JwtToken] = None
        self._refresh_token: Optional[JwtToken] = None

    def intercept_unary_stream(self, continuation, client_call_details, request):
        if self._needs_authentication():
            self._authenticate()

        client_call_details = self._insert_headers(
            [("authorization", f"Bearer {self._access_token.token}")],
            client_call_details,
        )

        return continuation(client_call_details, request)

    def intercept_stream_unary(
        self, continuation, client_call_details, request_iterator
    ):
        if self._needs_authentication():
            self._authenticate()

        client_call_details = self._insert_headers(
            [("authorization", f"Bearer {self._access_token.token}")],
            client_call_details,
        )

        return continuation(client_call_details, request_iterator)

    def intercept_stream_stream(
        self, continuation, client_call_details, request_iterator
    ):
        if self._needs_authentication():
            self._authenticate()

        client_call_details = self._insert_headers(
            [("authorization", f"Bearer {self._access_token.token}")],
            client_call_details,
        )

        return continuation(client_call_details, request_iterator)

    def intercept_unary_unary(self, continuation, client_call_details, request):
        if self._needs_authentication():
            self._authenticate()

        client_call_details = self._insert_headers(
            [("authorization", f"Bearer {self._access_token.token}")],
            client_call_details,
        )

        return continuation(client_call_details, request)

    @staticmethod
    def _insert_headers(
        new_metadata: List[Tuple[str, str]], client_call_details
    ) -> ClientCallDetails:
        metadata = []
        if client_call_details.metadata is not None:
            metadata = list(client_call_details.metadata)
        metadata.extend(new_metadata)

        return ClientCallDetails(
            client_call_details.method,
            client_call_details.timeout,
            metadata,
            client_call_details.credentials,
            False,
        )

    def _needs_authentication(self) -> bool:
        """
        The JWT needs authentication if there hasn't been an authentication yet or the access token has expired.
        """
        # return None in (
        #     self._refresh_token,
        #     self._access_token,
        # ) or self._access_token.expiration > int(time.time())
        return True

    def _authenticate(self):
        """
        Authenticate with the block engine.
        TODO (LB): don't always do full auth
        """
        credentials = ssl_channel_credentials()
        channel = secure_channel(self._url, credentials)
        auth_client = AuthServiceStub(channel)

        challenge = auth_client.GenerateAuthChallenge(
            GenerateAuthChallengeRequest(
                role=Role.SEARCHER, pubkey=bytes(self._kp.pubkey())
            )
        ).challenge
        print(f"{challenge=}")

        challenge_to_sign = f"{str(self._kp.pubkey())}-{challenge}"
        print(f"{challenge_to_sign=}")

        signed = self._kp.sign_message(bytes(challenge_to_sign, "utf8"))
        print(f"{signed=}")

        auth_tokens_response: GenerateAuthTokensResponse = (
            auth_client.GenerateAuthTokens(
                GenerateAuthTokensRequest(
                    challenge=challenge_to_sign,
                    client_pubkey=bytes(self._kp.pubkey()),
                    signed_challenge=bytes(signed),
                )
            )
        )

        self._access_token = JwtToken(
            token=auth_tokens_response.access_token.value,
            expiration=auth_tokens_response.access_token.expires_at_utc.seconds,
        )

        self._refresh_token = JwtToken(
            token=auth_tokens_response.refresh_token.value,
            expiration=auth_tokens_response.refresh_token.expires_at_utc.seconds,
        )


def get_searcher_client(url: str, kp: Keypair) -> SearcherServiceStub:
    """
    Returns a Searcher Service client that intercepts requests and authenticates with the block engine.
    :param url: url of the block engine without http/https
    :param kp: keypair of the block engine
    :return: SearcherServiceStub which handles authentication on requests
    """
    credentials = ssl_channel_credentials()
    channel = secure_channel(url, credentials)
    intercepted_channel = intercept_channel(channel, SearcherInterceptor(url, kp))
    return SearcherServiceStub(intercepted_channel)
