# -*- coding: utf-8 -*-
# Generated by the protocol buffer compiler.  DO NOT EDIT!
# source: auth.proto
"""Generated protocol buffer code."""
from google.protobuf.internal import builder as _builder
from google.protobuf import descriptor as _descriptor
from google.protobuf import descriptor_pool as _descriptor_pool
from google.protobuf import symbol_database as _symbol_database
# @@protoc_insertion_point(imports)

_sym_db = _symbol_database.Default()


from google.protobuf import timestamp_pb2 as google_dot_protobuf_dot_timestamp__pb2


DESCRIPTOR = _descriptor_pool.Default().AddSerializedFile(b'\n\nauth.proto\x12\x04\x61uth\x1a\x1fgoogle/protobuf/timestamp.proto\"H\n\x1cGenerateAuthChallengeRequest\x12\x18\n\x04role\x18\x01 \x01(\x0e\x32\n.auth.Role\x12\x0e\n\x06pubkey\x18\x02 \x01(\x0c\"2\n\x1dGenerateAuthChallengeResponse\x12\x11\n\tchallenge\x18\x01 \x01(\t\"_\n\x19GenerateAuthTokensRequest\x12\x11\n\tchallenge\x18\x01 \x01(\t\x12\x15\n\rclient_pubkey\x18\x02 \x01(\x0c\x12\x18\n\x10signed_challenge\x18\x03 \x01(\x0c\"J\n\x05Token\x12\r\n\x05value\x18\x01 \x01(\t\x12\x32\n\x0e\x65xpires_at_utc\x18\x02 \x01(\x0b\x32\x1a.google.protobuf.Timestamp\"c\n\x1aGenerateAuthTokensResponse\x12!\n\x0c\x61\x63\x63\x65ss_token\x18\x01 \x01(\x0b\x32\x0b.auth.Token\x12\"\n\rrefresh_token\x18\x02 \x01(\x0b\x32\x0b.auth.Token\"2\n\x19RefreshAccessTokenRequest\x12\x15\n\rrefresh_token\x18\x01 \x01(\t\"?\n\x1aRefreshAccessTokenResponse\x12!\n\x0c\x61\x63\x63\x65ss_token\x18\x01 \x01(\x0b\x32\x0b.auth.Token*L\n\x04Role\x12\x0b\n\x07RELAYER\x10\x00\x12\x0c\n\x08SEARCHER\x10\x01\x12\r\n\tVALIDATOR\x10\x02\x12\x1a\n\x16SHREDSTREAM_SUBSCRIBER\x10\x03\x32\xa7\x02\n\x0b\x41uthService\x12\x62\n\x15GenerateAuthChallenge\x12\".auth.GenerateAuthChallengeRequest\x1a#.auth.GenerateAuthChallengeResponse\"\x00\x12Y\n\x12GenerateAuthTokens\x12\x1f.auth.GenerateAuthTokensRequest\x1a .auth.GenerateAuthTokensResponse\"\x00\x12Y\n\x12RefreshAccessToken\x12\x1f.auth.RefreshAccessTokenRequest\x1a .auth.RefreshAccessTokenResponse\"\x00\x62\x06proto3')

_builder.BuildMessageAndEnumDescriptors(DESCRIPTOR, globals())
_builder.BuildTopDescriptorsAndMessages(DESCRIPTOR, 'auth_pb2', globals())
if _descriptor._USE_C_DESCRIPTORS == False:

  DESCRIPTOR._options = None
  _ROLE._serialized_start=570
  _ROLE._serialized_end=646
  _GENERATEAUTHCHALLENGEREQUEST._serialized_start=53
  _GENERATEAUTHCHALLENGEREQUEST._serialized_end=125
  _GENERATEAUTHCHALLENGERESPONSE._serialized_start=127
  _GENERATEAUTHCHALLENGERESPONSE._serialized_end=177
  _GENERATEAUTHTOKENSREQUEST._serialized_start=179
  _GENERATEAUTHTOKENSREQUEST._serialized_end=274
  _TOKEN._serialized_start=276
  _TOKEN._serialized_end=350
  _GENERATEAUTHTOKENSRESPONSE._serialized_start=352
  _GENERATEAUTHTOKENSRESPONSE._serialized_end=451
  _REFRESHACCESSTOKENREQUEST._serialized_start=453
  _REFRESHACCESSTOKENREQUEST._serialized_end=503
  _REFRESHACCESSTOKENRESPONSE._serialized_start=505
  _REFRESHACCESSTOKENRESPONSE._serialized_end=568
  _AUTHSERVICE._serialized_start=649
  _AUTHSERVICE._serialized_end=944
# @@protoc_insertion_point(module_scope)