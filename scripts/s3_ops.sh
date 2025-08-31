#!/bin/bash

# S3 operations using mc (MinIO client)
# Get S3 credentials from environment
ENDPOINT=${FOLD_INTERNER_BLOB_ENDPOINT:-http://localhost:9000}
ACCESS_KEY=${FOLD_INTERNER_BLOB_ACCESS_KEY:-minioadmin}
SECRET_KEY=${FOLD_INTERNER_BLOB_SECRET_KEY:-minioadmin}
BUCKET=${FOLD_INTERNER_BLOB_BUCKET:-internerdata}

# Configure mc alias - suppress output but check for mc availability
if ! command -v mc >/dev/null 2>&1; then
    echo "Error: mc (MinIO client) not found. Please install mc client."
    exit 1
fi

mc alias set local "$ENDPOINT" "$ACCESS_KEY" "$SECRET_KEY" >/dev/null 2>&1 || {
    echo "Failed to configure mc alias. Check S3 credentials and endpoint."
    exit 1
}

case "$1" in
    "split")
        if [[ $# -ne 3 ]]; then
            echo "Usage: $0 split <s3_path> <delimiter>"
            exit 1
        fi
        s3_path="$2"
        delimiter="$3"
        
        # Extract key from s3://bucket/key format
        key=$(echo "$s3_path" | sed 's|s3://[^/]*/||')
        
        # Download, split, and upload parts
        temp_file="/tmp/s3_download_$$"
        mc cp "local/$BUCKET/$key" "$temp_file" 2>/dev/null
        
        if [[ -f "$temp_file" ]]; then
            # Split file by delimiter and upload parts
            part=0
            while IFS= read -r line; do
                if [[ -n "$line" ]]; then
                    echo "$line" | mc pipe "local/$BUCKET/${key}-part-${part}"
                    ((part++))
                fi
            done < <(cat "$temp_file" | tr "$delimiter" '\n')
            
            # Delete original file
            mc rm "local/$BUCKET/$key"
            rm -f "$temp_file"
            echo "Split S3 object $s3_path into $part parts, deleted original."
        else
            echo "Failed to download $s3_path"
            exit 1
        fi
        ;;
    "clean-small")
        if [[ $# -ne 2 ]]; then
            echo "Usage: $0 clean-small <size_bytes>"
            exit 1
        fi
        size_threshold="$2"
        
        echo "Deleted objects under $size_threshold bytes:"
        mc find "local/$BUCKET" --name "*" | \
        while read name; do
            size=$(mc stat "local/$BUCKET/$name" | grep "Size" | awk '{print $3}' || echo "0")
            if [[ "$size" -lt "$size_threshold" ]]; then
                mc rm "local/$BUCKET/$name"
                echo "$name ($size bytes)"
            fi
        done
        ;;
    *)
        echo "Usage: $0 {split|clean-small} [args...]"
        exit 1
        ;;
esac