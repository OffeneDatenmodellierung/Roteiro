-- FIXTURE — deliberately unsafe. Never executed. See ../README.md.

CREATE TABLE reports (
    id      BIGINT PRIMARY KEY,
    tenant  TEXT NOT NULL,
    body    TEXT
);

-- Hands over every current and future permission on the object.
GRANT ALL PRIVILEGES ON reports TO reporting_service;

-- Writes query results to the database server's filesystem.
SELECT id, tenant, body FROM reports INTO OUTFILE '/tmp/reports.csv';
