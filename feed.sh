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
# todo add more benchmarks 
# todo consider having follower write to DBQ instead of direct to DB
# todo look at option for enriching data with follower requqeue reason to avoid re-intersect and duplicate results
# todo k8s 