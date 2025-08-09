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
make logs