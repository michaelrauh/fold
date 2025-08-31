#!/bin/bash

# Database operations using direct PostgreSQL queries
# Get database connection details from environment or use defaults
DB_HOST=${FOLD_POSTGRES_HOST:-localhost}
DB_PORT=${FOLD_POSTGRES_PORT:-5432}
DB_NAME=${FOLD_POSTGRES_DB:-fold}
DB_USER=${FOLD_POSTGRES_USER:-postgres}
DB_PASSWORD=${FOLD_POSTGRES_PASSWORD:-password}

export PGPASSWORD="$DB_PASSWORD"

case "$1" in
    "size"|"length")
        # Get database size (number of orthos)
        count=$(psql -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB_NAME" -t -c \
            "SELECT COUNT(*) FROM orthos;" 2>/dev/null | tr -d ' ')
        echo "Database length: ${count:-0}"
        ;;
    "optimal")
        # Get optimal ortho
        optimal=$(psql -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB_NAME" -t -c \
            "SELECT payload FROM orthos WHERE is_optimal = true ORDER BY updated_at DESC LIMIT 1;" 2>/dev/null)
        if [[ -n "$optimal" && "$optimal" != "(0 rows)" ]]; then
            echo "Optimal Ortho: $optimal"
        else
            echo "No optimal Ortho found."
        fi
        ;;
    "version-counts")
        # Get counts of orthos per version
        echo "version	count"
        psql -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB_NAME" -t -c \
            "SELECT version, COUNT(*) FROM orthos GROUP BY version ORDER BY version;" 2>/dev/null | \
            sed 's/|/\t/g' | sed 's/^ *//' | sed '/^$/d'
        ;;
    *)
        echo "Usage: $0 {size|optimal|version-counts}"
        exit 1
        ;;
esac