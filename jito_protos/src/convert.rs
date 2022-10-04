use std::{
    cmp::min,
    net::{IpAddr, Ipv4Addr},
};

use bincode::serialize;
use solana_perf::packet::{Packet, PacketBatch, PACKET_DATA_SIZE};
use solana_sdk::{
    packet::{Meta, PacketFlags},
    transaction::VersionedTransaction,
};

use crate::packet::{
    Meta as ProtoMeta, Packet as ProtoPacket, PacketBatch as ProtoPacketBatch,
    PacketFlags as ProtoPacketFlags,
};

/// Converts a Solana packet to a protobuf packet
/// NOTE: the packet.data() function will filter packets marked for discard
pub fn packet_to_proto_packet(p: &Packet) -> Option<ProtoPacket> {
    Some(ProtoPacket {
        data: p.data(..)?.to_vec(),
        meta: Some(ProtoMeta {
            size: p.meta.size as u64,
            addr: p.meta.addr.to_string(),
            port: p.meta.port as u32,
            flags: Some(ProtoPacketFlags {
                discard: p.meta.discard(),
                forwarded: p.meta.forwarded(),
                repair: p.meta.repair(),
                simple_vote_tx: p.meta.is_simple_vote_tx(),
                tracer_packet: false,
            }),
            sender_stake: p.meta.sender_stake,
        }),
    })
}

pub fn packet_batches_to_proto_packets(
    batches: &[PacketBatch],
) -> impl Iterator<Item = ProtoPacket> + '_ {
    batches.iter().flat_map(|b| {
        b.iter().filter(|p| !p.meta.discard()).filter_map(|p| {
            Some(ProtoPacket {
                data: p.data(..)?.to_vec(),
                meta: Some(ProtoMeta {
                    size: p.meta.size as u64,
                    addr: p.meta.addr.to_string(),
                    port: p.meta.port as u32,
                    flags: Some(ProtoPacketFlags {
                        discard: p.meta.discard(),
                        forwarded: p.meta.forwarded(),
                        repair: p.meta.repair(),
                        simple_vote_tx: p.meta.is_simple_vote_tx(),
                        tracer_packet: false,
                    }),
                    sender_stake: p.meta.sender_stake,
                }),
            })
        })
    })
}

// converts from a protobuf packet to packet
pub fn proto_packet_to_packet(p: &ProtoPacket) -> Packet {
    let mut data = [0; PACKET_DATA_SIZE];
    let copy_len = min(data.len(), p.data.len());
    data[..copy_len].copy_from_slice(&p.data[..copy_len]);
    let mut packet = Packet::new(data, Default::default());
    if let Some(meta) = &p.meta {
        packet.meta.size = meta.size as usize;
        packet.meta.addr = meta.addr.parse().unwrap_or(UNKNOWN_IP);
        packet.meta.port = meta.port as u16;
        if let Some(flags) = &meta.flags {
            if flags.simple_vote_tx {
                packet.meta.flags.insert(PacketFlags::SIMPLE_VOTE_TX);
            }
            if flags.forwarded {
                packet.meta.flags.insert(PacketFlags::FORWARDED);
            }
            if flags.tracer_packet {
                packet.meta.flags.insert(PacketFlags::TRACER_PACKET);
            }
            if flags.repair {
                packet.meta.flags.insert(PacketFlags::REPAIR);
            }
        }
        packet.meta.sender_stake = meta.sender_stake;
    }
    packet
}

const UNKNOWN_IP: IpAddr = IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0));
pub fn proto_packet_batch_to_packets(
    packet_batch: ProtoPacketBatch,
) -> impl Iterator<Item = Packet> {
    packet_batch.packets.into_iter().map(|proto_packet| {
        let mut packet = Packet::new([0; PACKET_DATA_SIZE], Meta::default());
        let copy_len = min(PACKET_DATA_SIZE, proto_packet.data.len());
        packet.buffer_mut()[..copy_len].copy_from_slice(&proto_packet.data[..copy_len]);
        if let Some(meta) = &proto_packet.meta {
            packet.meta.size = meta.size as usize;
            packet.meta.addr = meta.addr.parse().unwrap_or(UNKNOWN_IP);
            packet.meta.port = meta.port as u16;
            if let Some(flags) = &meta.flags {
                if flags.simple_vote_tx {
                    packet.meta.flags.insert(PacketFlags::SIMPLE_VOTE_TX);
                }
                if flags.forwarded {
                    packet.meta.flags.insert(PacketFlags::FORWARDED);
                }
                if flags.tracer_packet {
                    packet.meta.flags.insert(PacketFlags::TRACER_PACKET);
                }
                if flags.repair {
                    packet.meta.flags.insert(PacketFlags::REPAIR);
                }
                if flags.discard {
                    packet.meta.flags.insert(PacketFlags::DISCARD);
                }
            }
            packet.meta.sender_stake = meta.sender_stake;
        }
        packet
    })
}

/// Converts a protobuf packet to a VersionedTransaction
pub fn versioned_tx_from_packet(p: &ProtoPacket) -> Option<VersionedTransaction> {
    let mut data = [0; PACKET_DATA_SIZE];
    let copy_len = min(data.len(), p.data.len());
    data[..copy_len].copy_from_slice(&p.data[..copy_len]);
    let mut packet = Packet::new(data, Default::default());
    if let Some(meta) = &p.meta {
        packet.meta.size = meta.size as usize;
    }
    packet.deserialize_slice(..).ok()
}

/// Converts a VersionedTransaction to a protobuf packet
pub fn proto_packet_from_versioned_tx(tx: &VersionedTransaction) -> ProtoPacket {
    let data = serialize(tx).expect("serializes");
    let size = data.len() as u64;
    ProtoPacket {
        data,
        meta: Some(ProtoMeta {
            size,
            addr: "".to_string(),
            port: 0,
            flags: None,
            sender_stake: 0,
        }),
    }
}

#[cfg(test)]
mod tests {
    use solana_perf::test_tx::test_tx;
    use solana_sdk::transaction::VersionedTransaction;

    use crate::convert::{proto_packet_from_versioned_tx, versioned_tx_from_packet};

    #[test]
    fn test_proto_to_packet() {
        let tx_before = VersionedTransaction::from(test_tx());
        let tx_after = versioned_tx_from_packet(&proto_packet_from_versioned_tx(&tx_before))
            .expect("tx_after");

        assert_eq!(tx_before, tx_after);
    }
}
