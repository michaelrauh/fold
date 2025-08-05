#!/bin/bash
set -e

make up

make put-s3 FILE=e.txt

echo "Splitting e.txt by CHAPTER..."
make split FILE=e.txt DELIM="CHAPTER"

PARTS=$(make list-s3 | grep 'e.txt-part-' | awk '{print $4}')
for PART in $PARTS; do
    echo "Feeding $PART..."
    make feed FILE=$PART
    for i in {1..300}; do
        sleep 1
        echo "[Status] $(date):"
        make db-count
        make queue-count
    done
done
