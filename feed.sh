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

# todo throttling and auto-scaling
# todo managed DBs 
# todo persistent queues
# todo add more benchmarks 
# todo k8s 
# todo results explorer:
# - start empty and show all possible fills for each possible direction
# - indicate directions toward optimal
# - allow for rotating and showing different points of view in single orthos