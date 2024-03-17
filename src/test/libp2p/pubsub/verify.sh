#!/usr/bin/env bash

set -euo pipefail

echo "Test libp2p Ethereum pubsub"

NUM_VALIDATORS=10
NUM_SLOTS=25
SLOTS_PER_EPOCH=4
NUM_EPOCHS=$(($NUM_SLOTS / $SLOTS_PER_EPOCH))
MAX_COMMITTEES_PER_SLOT=2
TARGET_AGGREGATORS_PER_COMMITTEE=1

### Block proposing

# Verify that the block proposals are done correctly
# Skip the slot 0, because it's the start of the simulation and the proposer probably cannot make it on time
for slot in $(seq 1 $(($NUM_SLOTS - 1))); do
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
    # Skip the slot 0, because it's the start of the simulation and the proposer probably cannot make it on time
    if [[ "$(($published + $received))" != "$(($NUM_SLOTS - 1))" ]]; then
        printf "The number of blocks received and published on validator $vid is incorrect"
        exit 1
    fi
done

### Attestation and Aggregation

# Verify that all the validators attest in every epoch
# Skip the first epoch
for epoch in $(seq 1 $(($NUM_EPOCHS - 1))); do
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

# 1. Verify that all the validators are evenly distributed to subnets/committees
# 2. Verify that all the aggregations are done correctly
MIN_NUM_OF_MEMBERS=$(($NUM_VALIDATORS / ( $MAX_COMMITTEES_PER_SLOT * $SLOTS_PER_EPOCH )))
MAX_NUM_OF_MEMBERS=$(($MIN_NUM_OF_MEMBERS + 1))
for slot in $(seq 1 $(($NUM_SLOTS - 1))); do
    for cmid in $(seq 0 $(($MAX_COMMITTEES_PER_SLOT - 1))); do
        num_members=0
        num_aggregators=0
        for vid in $(seq 0 $(($NUM_VALIDATORS - 1))); do
            if cat ./hosts/peer$vid/*.stdout | grep \
                "Published to beacon_attestation_.*\"slot\":$slot\>.*\"cmid\":$cmid\>" >/dev/null
            then
                num_members=$(($num_members + 1))
            fi
            line=$(cat ./hosts/peer$vid/*.stdout | \
                grep "Published to beacon_aggregate_and_proof.*\"slot\":$slot\>.*\"cmid\":$cmid\>" || true)
            if ! [[ -z "$line" ]]; then
                num_aggregators=$(($num_aggregators + 1))
                if [[ $(echo "$line" | wc -l) != 1 ]]; then
                    printf "Validator $vid at slot $slot aggregated more than once"
                    exit 1
                fi
                attested_block_slot=$(echo "$line" | grep -o "\"block_slot\":[0-9]*" | cut -d ":" -f 2)
                if [[ "$attested_block_slot" != "$slot" ]]; then
                    printf "The attested block in the aggregate is not the current slot"
                    exit 1
                fi
            fi
        done
        if [[ $num_members < $MIN_NUM_OF_MEMBERS || $num_members > $MAX_COMMITTEES_PER_SLOT ]]; then
            printf "The number of members of committee $cmid at slot $slot is incorrect"
            exit 1
        fi
        # Take the minimum of the number of members and the target number of aggregators
        expect_num_aggregators=$(($TARGET_AGGREGATORS_PER_COMMITTEE > $num_members ? \
            $num_members : $TARGET_AGGREGATORS_PER_COMMITTEE))
        if [[ $expect_num_aggregators != $num_aggregators ]]; then
            printf "The number of aggregators of committee $cmid at slot $slot is incorrect"
            exit 1
        fi
    done
done

echo "Verification succeeded"
