#!/usr/bin/env bash

set -euo pipefail

echo "Test libp2p Ethereum pubsub"

NUM_VALIDATORS=10
NUM_SLOTS=20
SLOTS_PER_EPOCH=4
NUM_EPOCHS=$(($NUM_SLOTS / $SLOTS_PER_EPOCH))
MAX_COMMITTEES_PER_SLOT=2

### Block proposing

# Verify that the block proposals are done correctly
for slot in $(seq 0 $(($NUM_SLOTS - 1))); do
    num_published=0
    proposer_id=
    proposer_msgid=
    for vid in $(seq 0 $(($NUM_VALIDATORS - 1))); do
        line=$(cat ./hosts/peer$vid/*.stdout | grep "Published to beacon_block .*\"slot\":$slot\>" || true)
        if ! [[ -z "$line" ]]; then
            proposer_id=$vid
            proposer_msgid=$(echo "$line" | grep -o "msgid=[^ ]*" | cut -d '=' -f 2)
            num_published=$(($num_published + 1))
        fi
    done
    if [[ "$num_published" != 1 || -z "$proposer_msgid" ]]; then
        printf "Some slot has zero or more than one proposers"
        exit 1
    fi
    for vid in $(seq 0 $(($NUM_VALIDATORS - 1))); do
        if [[ $proposer_id == $vid ]]; then
            continue
        fi
        line=$(cat ./hosts/peer$vid/*.stdout | grep "Got block from slot=$slot vid=$proposer_id" || true)
        if [[ -z "$line" ]]; then
            printf "Validator $vid didn't get a block at slot $slot"
            exit 1
        fi
        received_msgid=$(echo "$line" | grep -o "msgid=[^ ]*" | cut -d '=' -f 2)
        if [[ "$received_msgid" != "$proposer_msgid" ]]; then
            printf "Validator $vid received an incorrect block message at slot $slot"
            exit 1
        fi
    done
done

# Verify that each validator receives the correct number of blocks
for vid in $(seq 0 $(($NUM_VALIDATORS - 1))); do
    published=$(cat ./hosts/peer$vid/*.stdout | grep "Published to beacon_block" | wc -l || true)
    received=$(cat ./hosts/peer$vid/*.stdout | grep "Got block from" | wc -l || true)
    if [[ "$(($published + $received))" != "$NUM_SLOTS" ]]; then
        printf "The number of blocks received and published on validator $vid is incorrect"
        exit 1
    fi
done

### Attestation

# Verify that all the validators attest in every epoch
for epoch in $(seq 0 $(($NUM_EPOCHS - 1))); do
    slot_at_epoch=$(($epoch * $SLOTS_PER_EPOCH))
    for vid in $(seq 0 $(($NUM_VALIDATORS - 1))); do
        attested=false
        for slot in $(seq $slot_at_epoch $(($slot_at_epoch + $SLOTS_PER_EPOCH - 1))); do
            line=$(cat ./hosts/peer$vid/*.stdout | grep "Published to beacon_attestation_.*\"slot\":$slot\>" || true)
            if [[ $(echo "$line" | wc -l) != 1 ]]; then
                printf "Validator $vid at slot $slot attested zero or more than once"
                exit 1
            fi
            if ! [[ -z "$line" ]]; then
                attested=true
                attested_block_slot=$(echo "$line" | grep -o "\"block_slot\":[0-9]*" | cut -d ":" -f 2)
                if [[ "$attested_block_slot" != "$slot" ]]; then
                    printf "The attested block is not the current slot"
                    exit 1
                fi
            fi
        done
        if [[ $attested = false ]]; then
            printf "Validator $vid didn't attest at epoch $epoch"
            exit 1
        fi
    done
done

# Verify that all the validators are evenly distributed to subnets/committees
MIN_NUM_OF_MEMBERS=$(($NUM_VALIDATORS / ( $MAX_COMMITTEES_PER_SLOT * $SLOTS_PER_EPOCH )))
MAX_NUM_OF_MEMBERS=$(($MIN_NUM_OF_MEMBERS + 1))
for slot in $(seq 0 $(($NUM_SLOTS - 1))); do
    for cmid in $(seq 0 $(($MAX_COMMITTEES_PER_SLOT - 1))); do
        num_members=0
        for vid in $(seq 0 $(($NUM_VALIDATORS - 1))); do
            if cat ./hosts/peer$vid/*.stdout | grep \
                "Published to beacon_attestation_.*\"slot\":$slot\>.*\"cmid\":$cmid\>" >/dev/null
            then
                num_members=$(($num_members + 1))
            fi
        done
        if [[ $num_members < $MIN_NUM_OF_MEMBERS || $num_members > $MAX_COMMITTEES_PER_SLOT ]]; then
            printf "The number of members of committee $cmid at slot $slot is incorrect"
            exit 1
        fi
    done
done

echo "Verification succeeded"
