import sys
import requests
from pathlib import Path
import random
import os
import json


if __name__ == '__main__':

    if len(sys.argv) != 6:
        print("Exprected arguments: n-triples file, output directory, endpoint url (read), update endpoint url, and graph store endpoint url")
        exit(1)
    
    # collect cmd arguments
    N_TRIPLES_FILE = sys.argv[1]
    OUTPUT_DIRECTORY = sys.argv[2]
    ENDPOINT_URL = sys.argv[3]
    UPDATE_ENDPOINT_URL = sys.argv[4]
    GRAPH_STORE_ENDPOINT_URL = sys.argv[5]

    # additional options (hard coded for now)
    N_WORKERS = 4
    N_OPERATIONS = 20
    POSSIBLE_OPERATIONS = 5
    DELETE_DATA_TEMPLATE_SIZE_MIN = 1  # min number of triples to be deleted by a DELETE DATA operation
    DELETE_DATA_TEMPLATE_SIZE_MAX = 10 # max number of triples to be deleted by a DELETE DATA operation
    INSERT_DATA_TEMPLATE_SIZE_MIN = 1  # min number of triples to be inserted by an INSERT DATA operation
    INSERT_DATA_TEMPLATE_SIZE_MAX = 10 # max number of triples to be inserted by an INSERT DATA operation
    TYPE_OF_SUBJECTS="<http://xmlns.com/foaf/0.1/Person>"
    INSERT_PREDICATE="<http://www.example.org/test>"
    INSERT_INTEGER=0

    def reset_endpoint(input_file):
        requests.post(url=UPDATE_ENDPOINT_URL, data="DROP ALL", headers={"Content-type": "application/sparql-update"})
        requests.post(url=UPDATE_ENDPOINT_URL, data=f"LOAD <file://{input_file}>", headers={"Content-type": "application/sparql-update"})

    def generate_triples(number_of_triple, subject, counter):
        ntriples = ""
        for i in range(0, number_of_triples):
            ntriples += f"{subject} {INSERT_PREDICATE} <http://www.example.org/test/{counter}> ."
            counter += 1
        return ntriples

    def validation_default_graph(subject):
        query = f"CONSTRUCT {{ {subject} ?p ?o }} WHERE {{ {subject} ?p ?o . FILTER(!isBlank(?o)) }}"
        return requests.get(url=ENDPOINT_URL, params={"query": query}, headers={"Accept": "text/plain"}).text.replace('\n', ' ')

    def validation_named_graph(subject):
        query = f"CONSTRUCT {{ {subject} ?p ?o }} WHERE {{ GRAPH {subject} {{ {subject} ?p ?o . FILTER(!isBlank(?o)) }} }}"
        return requests.get(url=ENDPOINT_URL, params={"query": query}, headers={"Accept": "text/plain"}).text.replace('\n', ' ')

    def insert_data(subject, graph):
        query_default = f"INSERT DATA {{ {graph} }}"
        query_named = f"INSERT DATA {{ GRAPH {subject} {{ {graph} }} }}"
        requests.post(url=UPDATE_ENDPOINT_URL, data=query_default, headers={"Content-type": "application/sparql-update"})
        requests.post(url=UPDATE_ENDPOINT_URL, data=query_named, headers={"Content-type": "application/sparql-update"})

    def gsp_post(subject, graph):
        requests.post(url=f"{GRAPH_STORE_ENDPOINT_URL}?default", data=graph, headers={"Content-type": "application/n-triples"})
        requests.post(url=f"{GRAPH_STORE_ENDPOINT_URL}?graph={subject[1:-1]}", data=graph, headers={"Content-type": "application/n-triples"})

    def gsp_put(subject, graph):
        requests.put(url=f"{GRAPH_STORE_ENDPOINT_URL}?graph={subject[1:-1]}", data=graph, headers={"Content-type": "application/n-triples"})
    
    def gsp_delete(subject):
        requests.delete(url=f"{GRAPH_STORE_ENDPOINT_URL}?graph={subject[1:-1]}")

    def get_triples_for_delete_data(subject, number_of_triples):
        query = f"CONSTRUCT {{ {subject} ?p ?o }} WHERE {{ {subject} ?p ?o . FILTER(!isBlank(?o)) }} LIMIT {number_of_triples}"

    def delete_data(number_of_triples):
        construct_query = f"CONSTRUCT {{ {subject} ?p ?o }} WHERE {{ {subject} ?p ?o . FILTER(!isBlank(?o)) }} LIMIT {number_of_triples}"
        graph = requests.get(url=ENDPOINT_URL, params={"query": construct_query}, headers={"Accept": "text/plain"}).text.replace('\n', ' ')
        query_default = f"DELETE DATA {{ {graph} }}"
        requests.post(url=UPDATE_ENDPOINT_URL, data=query_default, headers={"Content-type": "application/sparql-update"})


    # delete existing data and load the ntriples file in the remote endpoint
    reset_endpoint(os.path.abspath(N_TRIPLES_FILE))

    # get all unique subjects of the provided graph
    subjects = list()
    with open(N_TRIPLES_FILE, "r") as input_graph:
        for line in input_graph:
            triple = line.split(' ')
            # skip triples that are not ?s ?p <http://xmlns.com/foaf/0.1/Person>
            if len(triple) < 3 or triple[2] != TYPE_OF_SUBJECTS:
                continue
            subjects.append(triple[0])

    sujbect_index = 0

    for w_idx in range(0, N_WORKERS):
        # create worker directory
        worker_dir = f"{OUTPUT_DIRECTORY}/worker_{w_idx}"
        Path(worker_dir).mkdir(parents=True, exist_ok=True)
        for o_idx in range(0, N_OPERATIONS):
            # randomly select an operation
            op = None
            if sujbect_index + o_idx == 0:
                # first subject, always start with insert data
                op = 0
            else:
                op = random.randrange(0, POSSIBLE_OPERATIONS)

            ntriples_for_operation = ""
            operation_name = ""
            subject = ""
            
            # INSERT DATA
            if op == 0:
                subject = subjects[sujbect_index + o_idx]
                number_of_triples = random.randrange(INSERT_DATA_TEMPLATE_SIZE_MIN, INSERT_DATA_TEMPLATE_SIZE_MAX)
                ntriples_for_operation = generate_triples(number_of_triples, subject, INSERT_INTEGER)
                INSERT_INTEGER += number_of_triples
                operation_name = "INSERT_DATA"
                insert_data(subject, ntriples_for_operation)
            
            # DELETE DATA
            elif op == 1:
                # the subject that will be selected has not been used before
                # we can get the triples to be removed using a construct query
                # this is done in the function `delete_data`
                subject = subjects[sujbect_index + o_idx]
                number_of_triples = random.randrange(DELETE_DATA_TEMPLATE_SIZE_MIN, DELETE_DATA_TEMPLATE_SIZE_MAX)
                operation_name = "DELETE_DATA"
                delete_data(number_of_triples)
            
            # Graph Store Protocol: POST
            elif op == 2:
                subject = subjects[sujbect_index + o_idx]
                number_of_triples = random.randrange(INSERT_DATA_TEMPLATE_SIZE_MIN, INSERT_DATA_TEMPLATE_SIZE_MAX)
                ntriples_for_operation = generate_triples(number_of_triples, subject, INSERT_INTEGER)
                INSERT_INTEGER += number_of_triples
                operation_name = "GSP_POST"
                gsp_post(subject, ntriples_for_operation)

            # Graph Store Protocol: PUT
            elif op == 3:
                # select and already used subject
                subject = subjects[random.randrange(0, sujbect_index+o_idx)]
                number_of_triples = random.randrange(INSERT_DATA_TEMPLATE_SIZE_MIN, INSERT_DATA_TEMPLATE_SIZE_MAX)
                ntriples_for_operation = generate_triples(number_of_triples, subject, INSERT_INTEGER)
                INSERT_INTEGER += number_of_triples
                operation_name = "GSP_PUT"
                gsp_put(subject, ntriples_for_operation)

            # Graph Store Protocol: DELETE
            elif op == 4:
                # select and already used subject
                subject = subjects[random.randrange(0, sujbect_index+o_idx)]
                operation_name = "GSP_DELETE"
                gsp_delete(subject)
                
            else:
                raise "Error: Operation number out of expected range"
            
            validation_ntriples_in_default_graph = validation_default_graph(subject)
            validation_ntriples_in_named_graph = validation_named_graph(subject)

            operantion_file = f"{worker_dir}/op_{o_idx}.json"
            json_as_dict = {
                "subject": subject,
                "operation": operation_name,
                "new_triples": ntriples_for_operation,
                "validation_default": validation_ntriples_in_default_graph,
                "validation_named": validation_ntriples_in_named_graph,
            }
            with open(operantion_file, "w") as output_file:
                output_file.write(json.dumps(json_as_dict, indent=4))

        sujbect_index += N_OPERATIONS

