#!/bin/bash
# Check age of all heartbeat files in in_process

echo "=== Heartbeat Ages ==="
echo ""

for heartbeat in fold_state/in_process/*/heartbeat; do
    if [ -f "$heartbeat" ]; then
        folder=$(dirname "$heartbeat")
        folder_name=$(basename "$folder")
        
        # Get file modification time in seconds since epoch
        if [[ "$OSTYPE" == "darwin"* ]]; then
            # macOS
            mod_time=$(stat -f %m "$heartbeat")
        else
            # Linux
            mod_time=$(stat -c %Y "$heartbeat")
        fi
        
        # Get current time in seconds since epoch
        now=$(date +%s)
        
        # Calculate age
        age=$((now - mod_time))
        
        # Format age
        if [ $age -lt 60 ]; then
            age_str="${age}s"
        elif [ $age -lt 3600 ]; then
            age_str="$((age / 60))m $((age % 60))s"
        else
            hours=$((age / 3600))
            mins=$(((age % 3600) / 60))
            age_str="${hours}h ${mins}m"
        fi
        
        echo "$folder_name: $age_str ago"
    fi
done

echo ""
echo "Grace period: 600s (10 minutes)"
