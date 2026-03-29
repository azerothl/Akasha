import sys
import os
import json

def main():
    # Load the existing JSON file if it exists
    json_path = os.path.join(os.getcwd(), 'data.json')
    if os.path.exists(json_path):
        with open(json_path, 'r', encoding='utf-8') as f:
            data = json.load(f)
    else:
        data = {}

    # Append the new key-value pair
    data["new_key"] = "new_value"

    # Write back to the same JSON file
    with open(json_path, 'w', encoding='utf-8') as f:
        json.dump(data, f, indent=4)

if __name__ == "__main__":
    main()