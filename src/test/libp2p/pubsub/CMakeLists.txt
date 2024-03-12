add_custom_target(libp2p-pubsub-eth-graph-file ALL
                COMMAND ${CMAKE_COMMAND}
                  -E copy
                  ${CMAKE_CURRENT_SOURCE_DIR}/graph.txt
                  ${CMAKE_CURRENT_BINARY_DIR}/graph.txt)

add_shadow_tests(BASENAME libp2p-pubsub-eth
                 SHADOW_CONFIG "${CMAKE_CURRENT_SOURCE_DIR}/eth.yaml"
                 POST_CMD "${CMAKE_CURRENT_SOURCE_DIR}/verify.sh"
                 CONFIGURATIONS extra)
