# PubSub

## Graph file

In order to create a random graph for the pubsub network, we use Wilson's algorithm
to generate a connected graph. The script is written in `gen_graph.py` file.

The usage is `./gen_graph.py [number-of-nodes] [number-of-edges]`. It generates
a connected graph with the given number of nodes and the given number of edges. The
algorithm is adopted from https://stackoverflow.com/a/14618505.

```bash
$ ./gen_graph.py 10 20 | tee graph.txt
0 9 5 7 8 4 1
1 7 5 9
2 3 8 6 4
3 7 4
4 5 8 7
5
6
7 9
8 9
9
```

## Shadow config file

There is a script called `gen_eth_config.py` to generate a shadow config file.

The usage is `./gen_eth_config.py [number-of-nodes]`.

```bash
$ ./gen_eth_config.py 4
general:
  model_unblocked_syscall_latency: true
  stop_time: 1 hour
hosts:
  peer0: ...
  peer1: ...
  peer2: ...
  peer3: ...
network:
  graph:
    type: 1_gbit_switch
```
