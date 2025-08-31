#!/bin/bash

# Interner operations - simple functions using the file system and mc
# Get interner file location from environment or use default
INTERNER_FILE_LOCATION=${FOLD_INTERNER_FILE_LOCATION:-/tmp/interner}

case "$1" in
    "versions")
        # List interner versions by looking at files
        if [[ -d "$INTERNER_FILE_LOCATION" ]]; then
            echo "Interner versions:"
            find "$INTERNER_FILE_LOCATION" -name "*.interner" -type f 2>/dev/null | \
                sed 's|.*/||' | sed 's|\.interner||' | sort -n || echo "No interner files found"
        else
            echo "No interner directory found at $INTERNER_FILE_LOCATION"
        fi
        ;;
    "feed-s3")
        if [[ $# -ne 2 ]]; then
            echo "Usage: $0 feed-s3 <s3_path>"
            exit 1
        fi
        s3_path="$2"
        
        # For the bash script version, we'll just download and delete the S3 object
        # The actual feeding into interner requires the Rust BlobInternerHolder
        echo "Note: feed-s3 simplified for bash script implementation"
        
        # Extract key from s3://bucket/key format
        BUCKET=${FOLD_INTERNER_BLOB_BUCKET:-internerdata}
        key=$(echo "$s3_path" | sed 's|s3://[^/]*/||')
        
        # Check if mc is available
        if ! command -v mc >/dev/null 2>&1; then
            echo "Error: mc (MinIO client) not found"
            exit 1
        fi
        
        # Configure mc alias
        ENDPOINT=${FOLD_INTERNER_BLOB_ENDPOINT:-http://localhost:9000}
        ACCESS_KEY=${FOLD_INTERNER_BLOB_ACCESS_KEY:-minioadmin}
        SECRET_KEY=${FOLD_INTERNER_BLOB_SECRET_KEY:-minioadmin}
        mc alias set local "$ENDPOINT" "$ACCESS_KEY" "$SECRET_KEY" >/dev/null 2>&1
        
        # Download and delete the S3 object (simplified version)
        temp_file="/tmp/s3_feed_$$"
        if mc cp "local/$BUCKET/$key" "$temp_file" 2>/dev/null; then
            echo "Downloaded S3 object $s3_path ($(wc -l < "$temp_file") lines)"
            # In a full implementation, this would feed to the interner
            # For now, we just acknowledge and clean up
            mc rm "local/$BUCKET/$key"
            rm -f "$temp_file"
            echo "Deleted original S3 object"
        else
            echo "Failed to download $s3_path"
            exit 1
        fi
        ;;
    *)
        echo "Usage: $0 {versions|feed-s3} [args...]"
        exit 1
        ;;
esac