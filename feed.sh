#!/bin/bash
set -e
make list-s3
make put-s3 FILE=e.txt
make split FILE=e.txt DELIM=CHAPTER
make list-s3
make clean-s3-small SIZE=100
make list-s3
make feed-s3 FILE=e.txt-part-56
make db-count
make queue-count
make interner-versions
sleep 10
make feed-s3 FILE=e.txt-part-55
make db-count
make queue-count
make scale-worker REPLICAS=50
sleep 10
make db-count
make queue-count
make optimal
make logs

# todo make a flow that feeds periodically, ideally from a folder
# todo check results by calling optimal periodically and ensuring all subphrases are in the source text
# todo add some ability to track redundancy rate
# todo count number of orthos per version
# todo make a LRU that sits in the follower to lower DB pressure
# todo dedup in the follower and measure effectiveness
# todo add more benchmarks 
# todo k8s 