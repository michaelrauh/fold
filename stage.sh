#!/bin/bash
# stage.sh - Split a file by delimiter and move chunks to input folder
#
# Usage: ./stage.sh <input_file> <delimiter> [max_length] [state_dir]
#   input_file  - Path to the file to split
#   delimiter   - Word or phrase to split on (e.g., "chapter", "CHAPTER")
#   max_length  - Optional: Maximum length of each chunk in characters (default: unlimited)
#   state_dir   - Optional: State directory path (default: ./fold_state)
#
# Example: ./stage.sh book.txt "CHAPTER" 50000 ./fold_state

set -e

# Parse arguments
INPUT_FILE="${1:-}"
DELIMITER="${2:-}"
MAX_LENGTH="${3:-0}"
STATE_DIR="${4:-./fold_state}"

INPUT_DIR="${STATE_DIR}/input"

# Validate arguments
if [ -z "$INPUT_FILE" ]; then
    echo "Error: No input file specified"
    echo "Usage: $0 <input_file> <delimiter> [max_length] [state_dir]"
    exit 1
fi

if [ ! -f "$INPUT_FILE" ]; then
    echo "Error: Input file not found: $INPUT_FILE"
    exit 1
fi

if [ -z "$DELIMITER" ]; then
    echo "Error: No delimiter specified"
    echo "Usage: $0 <input_file> <delimiter> [max_length] [state_dir]"
    exit 1
fi

# Create input directory if it doesn't exist
mkdir -p "$INPUT_DIR"

echo "[stage] Input file: $INPUT_FILE"
echo "[stage] Delimiter: $DELIMITER"
echo "[stage] Max length: ${MAX_LENGTH} chars (0 = unlimited)"
echo "[stage] State directory: $STATE_DIR"
echo "[stage] Input directory: $INPUT_DIR"
echo ""

# Get the base name of the input file (without path and extension)
BASENAME=$(basename "$INPUT_FILE" .txt)

# Split the file using awk
awk -v delimiter="$DELIMITER" -v max_len="$MAX_LENGTH" -v output_dir="$INPUT_DIR" -v basename="$BASENAME" '
BEGIN {
    chunk = 0
    current_length = 0
    filename = ""
}

# Check if line contains delimiter (case-insensitive)
tolower($0) ~ tolower(delimiter) {
    # If we have accumulated content, write it out
    if (current_length > 0 && filename != "") {
        close(filename)
    }
    
    # Start new chunk
    chunk++
    filename = sprintf("%s/%s_chunk_%04d.txt", output_dir, basename, chunk)
    current_length = 0
    
    # Write the delimiter line to the new chunk
    print $0 > filename
    current_length += length($0) + 1
    next
}

# Regular line
{
    # If no chunk started yet, start the first one
    if (filename == "") {
        chunk = 1
        filename = sprintf("%s/%s_chunk_%04d.txt", output_dir, basename, chunk)
        current_length = 0
    }
    
    # Check if adding this line would exceed max_length
    if (max_len > 0 && current_length > 0 && (current_length + length($0) + 1) > max_len) {
        # Close current chunk and start a new one
        close(filename)
        chunk++
        filename = sprintf("%s/%s_chunk_%04d.txt", output_dir, basename, chunk)
        current_length = 0
    }
    
    # Write line to current chunk
    print $0 > filename
    current_length += length($0) + 1
}

END {
    if (filename != "") {
        close(filename)
    }
    print "[stage] Created " chunk " chunks" > "/dev/stderr"
}
' "$INPUT_FILE"

# Count the created files
NUM_FILES=$(ls -1 "$INPUT_DIR"/${BASENAME}_chunk_*.txt 2>/dev/null | wc -l)

echo ""
echo "[stage] Successfully created $NUM_FILES files in $INPUT_DIR"
echo "[stage] Files ready for processing"
echo ""

# Optionally delete the input file
read -p "[stage] Delete original file $INPUT_FILE? (y/N) " -n 1 -r
echo
if [[ $REPLY =~ ^[Yy]$ ]]; then
    rm "$INPUT_FILE"
    echo "[stage] Deleted original file: $INPUT_FILE"
else
    echo "[stage] Original file kept: $INPUT_FILE"
fi

echo "[stage] Done!"
