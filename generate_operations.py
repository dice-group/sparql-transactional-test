import argparse
import dataclasses
import enum
import json
import os
import random
from email.policy import default

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
GRAPH_SUBJECT: rdflib.URIRef = rdflib.URIRef("http://www.example.org/graph")

Graph = str
Query = str

def serialize_graph(g: rdflib.Graph) -> str:
    return g.serialize(format="nt")

class OperationKind(enum.Enum):
    INSERT_DATA = 0
    DELETE_DATA = 1
    GSP_POST = 2
    GSP_PUT = 3
    GSP_DELETE = 4

class Method(enum.Enum):
    POST = 0
    PUT = 1
    DELETE = 2

class Endpoint(enum.Enum):
    UPDATE = 0
    GSP = 1

@dataclasses.dataclass
class Validate:
    query: Query
    expected: Graph

@dataclasses.dataclass
class UpdateOperation:
    endpoint: Endpoint
    query_params: typing.Mapping[str, str]
    headers: typing.Mapping[str, str]
    method: Method
    body: str
    validate: Validate

    def to_json(self):
        return json.dumps({
            "endpoint": self.endpoint.name,
            "query_params": self.query_params,
            "headers": self.headers,
            "method": self.method.name,
            "body": self.body,
            "validate": {
                "query": self.validate.query,
                "expected": self.validate.expected,
            }
        }, indent=4)

class RDFStore:
    def __init__(self, query_endpoint_url: str, update_endpoint_url: str, graph_store_endpoint_url: str):
        self.query_endpoint_url = query_endpoint_url
        self.update_endpoint_url = update_endpoint_url
        self.graph_store_endpoint_url = graph_store_endpoint_url

    @staticmethod
    def _get(**kwargs) -> str:
        resp = requests.get(**kwargs)
        resp.raise_for_status()
        return resp.text

    @staticmethod
    def _post(**kwargs):
        requests.post(**kwargs).raise_for_status()

    @staticmethod
    def _put(**kwargs):
        requests.put(**kwargs).raise_for_status()

    @staticmethod
    def _delete(**kwargs):
        res = requests.delete(**kwargs)
        if res.status_code == 200 or res.status_code == 404:
            # both 200 and 404 mean that the graph already was or did get deleted
            return

        res.raise_for_status()

    def validation_default_graph(self, ident: rdflib.URIRef) -> Validate:
        query = (f"CONSTRUCT {{\n"
                 f"    {ident.n3()} ?p ?o\n"
                 f"}}\n"
                 f"WHERE {{\n"
                 f"    {ident.n3()} ?p ?o .\n"
                 f"    FILTER(!isBlank(?o))\n"
                 f"}}")
        res = self._get(url=self.query_endpoint_url, params={"query": query}, headers={"Accept": "application/n-triples"})

        return Validate(query=query, expected=res)

    def validation_named_graph(self, ident: rdflib.URIRef) -> Validate:
        query = (f"CONSTRUCT {{\n"
                 f"    {ident.n3()} ?p ?o\n"
                 f"}}\n"
                 f"WHERE {{\n"
                 f"    GRAPH {ident.n3()} {{ {GRAPH_SUBJECT.n3()} ?p ?o . FILTER(!isBlank(?o)) }}\n"
                 f"}}")
        res = self._get(url=self.query_endpoint_url, params={"query": query}, headers={"Accept": "application/n-triples"})

        return Validate(query=query, expected=res)

    def validation_default_and_named_graph(self, ident: rdflib.URIRef) -> Validate:
        query = (f"CONSTRUCT {{\n"
                 f"    {ident.n3()} ?pd ?od .\n"
                 f"    {GRAPH_SUBJECT.n3()} ?pn ?on .\n"
                 f"}}\n"
                 f"WHERE {{\n"
                 f"    {{ {ident.n3()} ?pd ?od . FILTER(!isBlank(?od)) }}\n"
                 f"    UNION"
                 f"    {{ GRAPH {ident.n3()} {{ {GRAPH_SUBJECT.n3()} ?pn ?on . FILTER(!isBlank(?on)) }} }}\n"
                 f"}}")
        res = self._get(url=self.query_endpoint_url, params={"query": query}, headers={"Accept": "application/n-triples"})

        return Validate(query=query, expected=res)


    def reset(self, triples_file: str):
        self._post(url=self.update_endpoint_url, data="DROP ALL", headers={"Content-type": "application/sparql-update"})

        with open(triples_file, "rb") as f:
            self._put(url=self.graph_store_endpoint_url, params="default", data=f,
                      headers={"Content-Type": "application/n-triples"})

    def insert_data(self, ident: rdflib.URIRef, triples: rdflib.Graph) -> UpdateOperation:
        named_triples = rdflib.Graph()
        for s, p, o in triples:
            named_triples.add((GRAPH_SUBJECT, p, o))

        update = (f"INSERT DATA {{ {serialize_graph(triples)} }};"
                  f"INSERT DATA {{ GRAPH {ident.n3()} {{ {serialize_graph(named_triples)} }} }}")

        headers = {"Content-type": "application/sparql-update"}

        self._post(url=self.update_endpoint_url, data=update,
                   headers=headers)

        return UpdateOperation(endpoint=Endpoint.UPDATE,
                               query_params={},
                               headers=headers,
                               method=Method.POST,
                               body=update,
                               validate=self.validation_default_and_named_graph(ident))

    def delete_data(self, ident: rdflib.URIRef, number_of_triples: int) -> UpdateOperation:
        construct_query = f"CONSTRUCT {{ {ident.n3()} ?p ?o }} WHERE {{ {ident.n3()} ?p ?o . FILTER(!isBlank(?o)) }} LIMIT {number_of_triples}"
        graph = self._get(url=self.query_endpoint_url, params={"query": construct_query},
                         headers={"Accept": "application/n-triples"})

        update = f"DELETE DATA {{ {graph} }}"
        headers = {"Content-type": "application/sparql-update"}

        self._post(url=self.update_endpoint_url, data=update, headers=headers)

        return UpdateOperation(endpoint=Endpoint.UPDATE,
                               query_params={},
                               headers=headers,
                               method=Method.POST,
                               body=update,
                               validate=self.validation_default_graph(ident))

    def gsp_post(self, ident: rdflib.URIRef, triples: rdflib.Graph) -> UpdateOperation:
        params = {"graph": ident}
        headers = {"Content-type": "application/n-triples"}
        body = serialize_graph(triples)

        self._post(url=self.graph_store_endpoint_url, headers=headers, params=params, data=body)

        return UpdateOperation(endpoint=Endpoint.GSP,
                               query_params=params,
                               headers=headers,
                               method=Method.POST,
                               body=body,
                               validate=self.validation_default_and_named_graph(ident))

    def gsp_put(self, ident: rdflib.URIRef, triples: rdflib.Graph) -> UpdateOperation:
        params = {"graph": ident}
        headers = {"Content-type": "application/n-triples"}
        body = serialize_graph(triples)

        self._put(url=self.graph_store_endpoint_url, headers=headers, params=params, data=body)

        return UpdateOperation(endpoint=Endpoint.GSP,
                               query_params=params,
                               headers=headers,
                               method=Method.PUT,
                               body=body,
                               validate=self.validation_named_graph(ident))

    def gsp_delete(self, ident: rdflib.URIRef) -> UpdateOperation:
        params = {"graph": ident}

        self._delete(url=self.graph_store_endpoint_url, params=params)

        return UpdateOperation(endpoint=Endpoint.GSP,
                               query_params=params,
                               headers={},
                               method=Method.DELETE,
                               body="",
                               validate=self.validation_named_graph(ident))


class TripleGenerator:
    def __init__(self):
        self._counter = 0

    def generate_triples(self, number_of_triples: int, ident: rdflib.URIRef) -> rdflib.Graph:
        g = rdflib.Graph()
        for i in range(0, number_of_triples):
            g.add((ident, INSERT_PREDICATE, rdflib.URIRef(f"http://www.example.org/test/{self._counter}")))

        return g


def extract_subjects_from_graph(triples_file: str) -> typing.List[rdflib.URIRef]:
    g = rdflib.Graph()
    g.parse(triples_file)

    return [s for s, p, o in g if isinstance(s, rdflib.URIRef) and o == TYPE_OF_SUBJECTS]


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

    try:
        os.mkdir(args.output_directory)
    except:
        # allow directory exists
        pass

    gen = TripleGenerator()

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
                number_of_triples = random.randrange(INSERT_DATA_TEMPLATE_SIZE_MIN, INSERT_DATA_TEMPLATE_SIZE_MAX)
                triples = gen.generate_triples(number_of_triples, subject)

                operation = rdfstore.insert_data(subject, triples)
            
            # DELETE DATA
            elif op_kind == OperationKind.DELETE_DATA:
                subject = subjects[subject_index + o_idx]
                number_of_triples = random.randrange(DELETE_DATA_TEMPLATE_SIZE_MIN, DELETE_DATA_TEMPLATE_SIZE_MAX)

                operation = rdfstore.delete_data(subject, number_of_triples)
            
            # Graph Store Protocol: POST
            elif op_kind == OperationKind.GSP_POST:
                subject = subjects[subject_index + o_idx]
                number_of_triples = random.randrange(INSERT_DATA_TEMPLATE_SIZE_MIN, INSERT_DATA_TEMPLATE_SIZE_MAX)
                ntriples_for_operation = gen.generate_triples(number_of_triples, subject)

                operation = rdfstore.gsp_post(subject, ntriples_for_operation)

            # Graph Store Protocol: PUT
            elif op_kind == OperationKind.GSP_PUT:
                # select an already used subject
                subject = subjects[random.randrange(0, subject_index + o_idx)]
                number_of_triples = random.randrange(INSERT_DATA_TEMPLATE_SIZE_MIN, INSERT_DATA_TEMPLATE_SIZE_MAX)
                ntriples_for_operation = gen.generate_triples(number_of_triples, subject)

                operation = rdfstore.gsp_put(subject, ntriples_for_operation)

            # Graph Store Protocol: DELETE
            elif op_kind == OperationKind.GSP_DELETE:
                # select and already used subject
                subject = subjects[random.randrange(0, subject_index + o_idx)]

                operation = rdfstore.gsp_delete(subject)

            else:
                raise "Error: Operation number out of expected range"

            with open(f"{worker_dir}/op_{o_idx}.json", "w") as output_file:
                output_file.write(operation.to_json())

        subject_index += N_OPERATIONS
