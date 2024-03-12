#!/usr/bin/env python3

# Used to generate a Shadow config file used in the Ethereum gossipsub network

import ipaddress
import sys

if len(sys.argv) < 2:
    print("Usage: ./gen_eth_config.py [number-of-nodes]")
    exit(1)

num_nodes = int(sys.argv[1])

base_ip = ipaddress.ip_address('1.0.0.1')

hosts = dict()
for idx in range(num_nodes):
    hosts['peer'+str(idx)] =  {
        'network_node_id': 0,
        'ip_addr': str(ipaddress.ip_address(int(base_ip)+idx)),
        'processes': [
            {
                'path': '../../../target/debug/test_libp2p_eth',
                'args': ['../../../graph.txt', str(idx)],
                'expected_final_state': 'running',
            },
        ],
    }

config = {
    'general': {
        'stop_time': '5 min',
        'model_unblocked_syscall_latency': True,
    },
    'network': {
        'graph': {
            'type': '1_gbit_switch',
        },
    },
    'hosts': hosts,
}

import yaml
print(yaml.safe_dump(config))
