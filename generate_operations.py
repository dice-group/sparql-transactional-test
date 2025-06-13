import argparse
import dataclasses
import enum
import json
import os
import random
import rdflib
import requests
import typing

# additional options (hard coded for now)
N_WORKERS: int = 4
N_OPERATIONS: int = 20
DELETE_DATA_TEMPLATE_SIZE_MIN: int = 1  # min number of triples to be deleted by a DELETE DATA operation
DELETE_DATA_TEMPLATE_SIZE_MAX: int = 10  # max number of triples to be deleted by a DELETE DATA operation
INSERT_DATA_TEMPLATE_SIZE_MIN: int = 1  # min number of triples to be inserted by an INSERT DATA operation
INSERT_DATA_TEMPLATE_SIZE_MAX: int = 10  # max number of triples to be inserted by an INSERT DATA operation
TYPE_OF_SUBJECTS: rdflib.URIRef = rdflib.URIRef("http://xmlns.com/foaf/0.1/Person")
INSERT_PREDICATE: rdflib.URIRef = rdflib.URIRef("http://www.example.org/test")

Graph = str

class OperationKind(enum.Enum):
    INSERT_DATA = 0
    DELETE_DATA = 1
    GSP_POST = 2
    GSP_PUT = 3
    GSP_DELETE = 4

class EndpointKind(enum.Enum):
    update = 0
    gsp = 1

class GraphKind(enum.Enum):
    default = 0
    named = 1
    both = 2


@dataclasses.dataclass
class UpdateOperation:
    subject: rdflib.URIRef
    endpoint: EndpointKind
    operation: OperationKind
    query: GraphKind
    query_triples: Graph
    validate: GraphKind
    validate_triples: Graph

    def to_json(self):
        return json.dumps({
            "subject": self.subject.n3(),
            "endpoint": self.endpoint.name,
            "operation": self.operation.name,
            "query": self.query.name,
            "query_triples": self.query_triples,
            "validate": self.validate.name,
            "validate_triples": self.validate_triples
        }, indent=4)

class RDFStore:
    def __init__(self, query_endpoint_url: str, update_endpoint_url: str, graph_store_endpoint_url: str):
        self.query_endpoint_url = query_endpoint_url
        self.update_endpoint_url = update_endpoint_url
        self.graph_store_endpoint_url = graph_store_endpoint_url

    @staticmethod
    def get(**kwargs) -> str:
        resp = requests.get(**kwargs)
        resp.raise_for_status()
        return resp.text

    @staticmethod
    def post(**kwargs):
        requests.post(**kwargs).raise_for_status()

    @staticmethod
    def put(**kwargs):
        requests.put(**kwargs).raise_for_status()

    @staticmethod
    def delete(**kwargs):
        # do not check for error codes (some systems return 404 if the graph is not found)
        # todo: check for internal server errors
        requests.delete(**kwargs)

    def validation_default_graph(self, ident: rdflib.URIRef) -> str:
        query = f"CONSTRUCT {{ {ident.n3()} ?p ?o }} WHERE {{ {ident.n3()} ?p ?o . FILTER(!isBlank(?o)) }}"
        return self.get(url=self.query_endpoint_url, params={"query": query}, headers={"Accept": "application/n-triples"})

    def validation_named_graph(self, ident: rdflib.URIRef) -> str:
        query = f"CONSTRUCT {{ {ident.n3()} ?p ?o }} WHERE {{ GRAPH {ident.n3()} {{ {ident.n3()} ?p ?o . FILTER(!isBlank(?o)) }} }}"
        return self.get(url=self.query_endpoint_url, params={"query": query}, headers={"Accept": "application/n-triples"})

    def reset(self, triples_file: str):
        self.post(url=self.update_endpoint_url, data="DROP ALL", headers={"Content-type": "application/sparql-update"})

        with open(triples_file, "rb") as f:
            self.put(url=self.graph_store_endpoint_url, params="default", data=f,
                     headers={"Content-Type": "application/n-triples"})

    def insert_data(self, ident: rdflib.URIRef, triples: Graph):
        update = f"""
            INSERT DATA {{ {triples} }};
            INSERT DATA {{ GRAPH {ident.n3()} {{ {triples} }} }}
        """

        self.post(url=self.update_endpoint_url, data=update,
                  headers={"Content-type": "application/sparql-update"})

    def delete_data(self, ident: rdflib.URIRef, number_of_triples: int) -> str:
        construct_query = f"CONSTRUCT {{ {ident.n3()} ?p ?o }} WHERE {{ {ident.n3()} ?p ?o . FILTER(!isBlank(?o)) }} LIMIT {number_of_triples}"
        graph = self.get(url=self.query_endpoint_url, params={"query": construct_query},
                         headers={"Accept": "application/n-triples"})

        query_default = f"DELETE DATA {{ {graph} }}"
        self.post(url=self.update_endpoint_url, data=query_default,
                      headers={"Content-type": "application/sparql-update"})

        return graph

    def gsp_post(self, ident: rdflib.URIRef, triples: Graph):
        self.post(url=self.graph_store_endpoint_url, params={"graph": ident}, data=triples,
                  headers={"Content-type": "application/n-triples"})

    def gsp_put(self, ident: rdflib.URIRef, triples: Graph):
        self.put(url=self.graph_store_endpoint_url, params={"graph": ident}, data=triples,
                 headers={"Content-type": "application/n-triples"})

    def gsp_delete(self, ident: rdflib.URIRef):
        self.delete(url=self.graph_store_endpoint_url, params={"graph": ident})


def extract_subjects_from_graph(triples_file: str) -> typing.List[rdflib.URIRef]:
    g = rdflib.Graph()
    g.parse(triples_file)

    return [s for s, p, o in g if isinstance(s, rdflib.URIRef) and o == TYPE_OF_SUBJECTS]

def generate_triples(number_of_triples: int, ident: rdflib.URIRef, counter: int) -> Graph:
    g = rdflib.Graph()
    for i in range(0, number_of_triples):
        g.add((ident, INSERT_PREDICATE, rdflib.URIRef(f"http://www.example.org/test/{counter}")))

    return g.serialize(format="nt")

if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument("n_triples_file", type=str)
    parser.add_argument("output_directory", type=str)
    parser.add_argument("query_endpoint_url", type=str)
    parser.add_argument("update_endpoint_url", type=str)
    parser.add_argument("graph_store_endpoint_url", type=str)
    args = parser.parse_args()

    rdfstore = RDFStore(
        query_endpoint_url=args.query_endpoint_url,
        update_endpoint_url=args.update_endpoint_url,
        graph_store_endpoint_url=args.graph_store_endpoint_url,
    )

    # delete existing data and load the ntriples file in the remote endpoint
    rdfstore.reset(args.n_triples_file)

    # get all unique subjects of the provided graph
    subjects = extract_subjects_from_graph(args.n_triples_file)
    subject_index = 0

    insert_integer = 0

    try:
        os.mkdir(args.output_directory)
    except:
        # allow directory exists
        pass

    for w_idx in range(0, N_WORKERS):
        # create worker directory
        worker_dir = f"{args.output_directory}/worker_{w_idx}"
        os.mkdir(worker_dir)

        for o_idx in range(0, N_OPERATIONS):
            # randomly select an operation

            # first subject, always start with insert data
            op_kind = OperationKind.INSERT_DATA if subject_index + o_idx == 0 else random.choice(list(OperationKind))

            # INSERT DATA
            if op_kind == OperationKind.INSERT_DATA:
                subject = subjects[subject_index + o_idx]
                endpoint_kind = EndpointKind.update
                number_of_triples = random.randrange(INSERT_DATA_TEMPLATE_SIZE_MIN, INSERT_DATA_TEMPLATE_SIZE_MAX)
                ntriples_for_operation = generate_triples(number_of_triples, subject, insert_integer)
                query_graph_kind = GraphKind.both
                validate_graph_kind = GraphKind.default
                insert_integer += number_of_triples
                rdfstore.insert_data(subject, ntriples_for_operation)
            
            # DELETE DATA
            elif op_kind == OperationKind.DELETE_DATA:
                subject = subjects[subject_index + o_idx]
                endpoint_kind = EndpointKind.update
                number_of_triples = random.randrange(DELETE_DATA_TEMPLATE_SIZE_MIN, DELETE_DATA_TEMPLATE_SIZE_MAX)
                ntriples_for_operation = rdfstore.delete_data(subject, number_of_triples)
                query_graph_kind = GraphKind.default
                validate_graph_kind = GraphKind.default
            
            # Graph Store Protocol: POST
            elif op_kind == OperationKind.GSP_POST:
                subject = subjects[subject_index + o_idx]
                endpoint_kind = EndpointKind.gsp
                number_of_triples = random.randrange(INSERT_DATA_TEMPLATE_SIZE_MIN, INSERT_DATA_TEMPLATE_SIZE_MAX)
                ntriples_for_operation = generate_triples(number_of_triples, subject, insert_integer)
                insert_integer += number_of_triples
                rdfstore.gsp_post(subject, ntriples_for_operation)
                query_graph_kind = GraphKind.both
                validate_graph_kind = GraphKind.default

            # Graph Store Protocol: PUT
            elif op_kind == OperationKind.GSP_PUT:
                # select an already used subject
                subject = subjects[random.randrange(0, subject_index + o_idx)]
                endpoint_kind = EndpointKind.gsp
                number_of_triples = random.randrange(INSERT_DATA_TEMPLATE_SIZE_MIN, INSERT_DATA_TEMPLATE_SIZE_MAX)
                ntriples_for_operation = generate_triples(number_of_triples, subject, insert_integer)
                insert_integer += number_of_triples
                rdfstore.gsp_put(subject, ntriples_for_operation)
                query_graph_kind = GraphKind.named
                validate_graph_kind = GraphKind.named

            # Graph Store Protocol: DELETE
            elif op_kind == OperationKind.GSP_DELETE:
                # select and already used subject
                subject = subjects[random.randrange(0, subject_index + o_idx)]
                endpoint_kind = EndpointKind.gsp
                ntriples_for_operation = ""
                rdfstore.gsp_delete(subject)
                query_graph_kind = GraphKind.named
                validate_graph_kind = GraphKind.named

            else:
                raise "Error: Operation number out of expected range"
            
            if op_kind in [OperationKind.INSERT_DATA, OperationKind.DELETE_DATA, OperationKind.GSP_POST]:   
                ntriples_for_validation = rdfstore.validation_default_graph(subject)
            else:
                ntriples_for_validation = rdfstore.validation_named_graph(subject)

            operation = UpdateOperation(
                subject=subject,
                endpoint=endpoint_kind,
                operation=op_kind,
                query=query_graph_kind,
                query_triples=ntriples_for_operation,
                validate=validate_graph_kind,
                validate_triples=ntriples_for_validation
            )

            with open(f"{worker_dir}/op_{o_idx}.json", "w") as output_file:
                output_file.write(operation.to_json())

        subject_index += N_OPERATIONS
