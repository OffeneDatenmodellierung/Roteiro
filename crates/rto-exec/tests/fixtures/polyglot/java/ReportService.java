// FIXTURE — deliberately unsafe. Never compiled, never run. See ../README.md.
package fixtures;

import java.sql.ResultSet;
import java.sql.Statement;

public class ReportService {
    /** Command injection: the composed string reaches a shell. */
    public Process render(String templateName) throws Exception {
        return Runtime.getRuntime().exec("render --template " + templateName);
    }

    /** SQL injection: the query is built by concatenation. */
    public ResultSet forTenant(Statement statement, String tenant) throws Exception {
        return statement.executeQuery("SELECT * FROM reports WHERE tenant = '" + tenant + "'");
    }
}
