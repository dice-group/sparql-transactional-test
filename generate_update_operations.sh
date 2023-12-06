#!/bin/bash

set -euo pipefail

# creates a list of update operations (INSERT DATA or DELETE DATA) for a predefined number of workers
# generates a directory for each worker 
# each directory contains the the updated operations (.ru files), the validation files for each query (.nt files) and their corresponding subject (.txt files)

# cmd arguments
N_TRIPLES_FILE=$1 # path to RDF graph ; must be N-TRIPLES ;
OUTPUT_DIRECTORY=$2  # path to the directory that will store the workloads
# the triple store is used to carry out the update operations and generate the validation files
ENDPOINT_URL=$3
UPDATE_ENDPOINT_URL=$4

# additional arguments (hard coded for now)
N_WORKERS=4
N_OPERATIONS=100
DELETE_DATA_TEMPLATE_SIZE_MIN=1  # min number of triples to be deleted by a DELETE DATA operation
DELETE_DATA_TEMPLATE_SIZE_MAX=10 # max number of triples to be deleted by a DELETE DATA operation
INSERT_DATA_TEMPLATE_SIZE_MIN=1  # min number of triples to be inserted by an INSERT DATA operation
INSERT_DATA_TEMPLATE_SIZE_MAX=10 # max number of triples to be inserted by an INSERT DATA operation
TYPE_OF_SUBJECTS="<http://xmlns.com/foaf/0.1/Person>"
INSERT_PREDICATE="<http://www.example.org/test>"
INSERT_INTEGER_LITERAL=0

# get all unique subjects of the provided graph
subjects=$(grep -e $TYPE_OF_SUBJECTS $N_TRIPLES_FILE | awk '{print $1}' | sort -u)
# store them into an array
readarray -t subjects_array <<<"$subjects"
current_subject_counter=0

# iterate over the number of workers
for ((i=1;i<=N_WORKERS;i++));
do
    # create directory for the worker
    worker_dir=$OUTPUT_DIRECTORY/worker_"$i"
    mkdir -p $worker_dir
    # iterate over the number of operations
    for ((j=1;j<N_OPERATIONS;j++));
    do
        # array idx - used to access the subjects
        idx=$((current_subject_counter + j))
        # randomly select the number of triples that will be inserted / deleted
        template_size=$(shuf -i $DELETE_DATA_TEMPLATE_SIZE_MIN-$DELETE_DATA_TEMPLATE_SIZE_MAX -n 1) # get a random number for template size
        current_subject="${subjects_array[idx]}"
        # choose op randomly
        op=$(shuf -i 0-1 -n 1) 
        op_str=""
        if [[ "$op" == "0" ]]
        # delete
        then
            delete_template=$(cat $N_TRIPLES_FILE | grep "^$current_subject" | grep -Ev '> _:.+?\.$' | head -n $template_size | tr '\n' ' ' )
            op_str="DELETE DATA { $delete_template }"
            echo $op_str > "$worker_dir/op_$j.ru"
        # insert
        else
            insert_template=""
            for ((y=0;y<$template_size;y++));
            do
                insert_template="${insert_template}${current_subject} ${INSERT_PREDICATE} \"${INSERT_INTEGER_LITERAL}\" . "
                INSERT_INTEGER_LITERAL=$((INSERT_INTEGER_LITERAL + 1))
            done
            op_str="INSERT DATA { $insert_template }"
            echo $op_str > "$worker_dir/op_$j.ru"
        fi
        curl -X POST "$UPDATE_ENDPOINT_URL" -H 'Content-Type: application/sparql-update' -d "$op_str" 
        curl -G --data-urlencode "query=CONSTRUCT { $current_subject ?p ?o } WHERE { $current_subject ?p ?o . FILTER(!isBlank(?o)) }" "$ENDPOINT_URL" -H 'Accept: text/plain' > "$worker_dir/op_$j.nt"
        echo $current_subject > "$worker_dir/op_$j.txt"
    done
    # increase current subject counter by N_OPERATIONS -> no overlap between workers
    current_subject_counter=$((current_subject_counter + N_OPERATIONS))
done