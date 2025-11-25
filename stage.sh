#!/bin/bash
# stage.sh - Split a file by sentences (like Interner does) into input folder
#
# Usage: ./stage.sh <input_file> [min_length] [state_dir]
#   input_file  - Path to the file to split
#   min_length  - Optional: Minimum length in words to keep chunk (default: 2)
#   state_dir   - Optional: State directory path (default: ./fold_state)
#
# Chunks smaller than min_length are automatically deleted to filter out junk.
# The input file is always kept (not deleted).
#
# Splits text the same way the Interner finds sentences:
# - Splits on paragraph breaks (\n\n)
# - Then splits on sentence delimiters (. ? ; !)
#
# Example: ./stage.sh book.txt 10 ./fold_state

set -e

# Parse arguments
INPUT_FILE="${1:-}"
MIN_LENGTH="${2:-2}"
STATE_DIR="${3:-./fold_state}"

INPUT_DIR="${STATE_DIR}/input"

# Validate arguments
if [ -z "$INPUT_FILE" ]; then
    echo "Error: No input file specified"
    echo "Usage: $0 <input_file> [min_length] [state_dir]"
    exit 1
fi

if [ ! -f "$INPUT_FILE" ]; then
    echo "Error: Input file not found: $INPUT_FILE"
    exit 1
fi

# Create input directory if it doesn't exist
mkdir -p "$INPUT_DIR"

echo "[stage] Input file: $INPUT_FILE"
echo "[stage] Minimum length: ${MIN_LENGTH} words (chunks smaller will be deleted)"
echo "[stage] State directory: $STATE_DIR"
echo "[stage] Input directory: $INPUT_DIR"
echo "[stage] Splitting by sentences (like Interner)"
echo ""

# Get the base name of the input file (without path and extension)
BASENAME=$(basename "$INPUT_FILE" .txt)

# Split the file using awk - mimics Interner's sentence splitting
# First split by double newlines (paragraphs), then by sentence delimiters
LC_ALL=C awk -v output_dir="$INPUT_DIR" -v basename="$BASENAME" '
BEGIN {
    chunk = 0
    RS = ""  # Paragraph mode - double newline separates records
    ORS = ""
}

{
    # Process each paragraph
    paragraph = $0
    
    # Split by sentence delimiters: . ? ; ! ,
    # We need to handle each character and accumulate sentences
    sentence = ""
    for (i = 1; i <= length(paragraph); i++) {
        char = substr(paragraph, i, 1)
        
        if (char ~ /[.?;!,]/) {
            # Found a sentence delimiter
            if (length(sentence) > 0) {
                # Trim and write the sentence if not empty
                gsub(/^[ \t\n]+|[ \t\n]+$/, "", sentence)
                if (length(sentence) > 0) {
                    chunk++
                    filename = sprintf("%s/%s_chunk_%04d.txt", output_dir, basename, chunk)
                    print sentence "\n" > filename
                    close(filename)
                }
                sentence = ""
            }
        } else {
            sentence = sentence char
        }
    }
    
    # Handle any remaining sentence in the paragraph
    if (length(sentence) > 0) {
        gsub(/^[ \t\n]+|[ \t\n]+$/, "", sentence)
        if (length(sentence) > 0) {
            chunk++
            filename = sprintf("%s/%s_chunk_%04d.txt", output_dir, basename, chunk)
            print sentence "\n" > filename
            close(filename)
        }
    }
}

END {
    print "[stage] Created " chunk " chunks" > "/dev/stderr"
}
' "$INPUT_FILE"

# Count the created files
NUM_FILES=$(ls -1 "$INPUT_DIR"/${BASENAME}_chunk_*.txt 2>/dev/null | wc -l)

echo ""
echo "[stage] Successfully created $NUM_FILES chunks"

# Filter out small chunks if min_length is specified
if [ "$MIN_LENGTH" -gt 0 ]; then
    echo "[stage] Filtering chunks smaller than $MIN_LENGTH words..."
    
    DELETED_COUNT=0
    for file in "$INPUT_DIR"/${BASENAME}_chunk_*.txt; do
        if [ -f "$file" ]; then
            # Get file size in words
            FILE_SIZE=$(wc -w < "$file")
            
            if [ "$FILE_SIZE" -lt "$MIN_LENGTH" ]; then
                echo "[stage] Deleting small chunk: $(basename "$file") ($FILE_SIZE words)"
                rm "$file"
                DELETED_COUNT=$((DELETED_COUNT + 1))
            fi
        fi
    done
    
    REMAINING_FILES=$(ls -1 "$INPUT_DIR"/${BASENAME}_chunk_*.txt 2>/dev/null | wc -l)
    echo "[stage] Deleted $DELETED_COUNT small chunks"
    echo "[stage] Remaining files: $REMAINING_FILES"
else
    REMAINING_FILES=$NUM_FILES
fi

echo ""
echo "[stage] $REMAINING_FILES files ready for processing in $INPUT_DIR"
echo "[stage] Input file kept: $INPUT_FILE"
echo "[stage] Done!"
