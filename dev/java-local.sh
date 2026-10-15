#!/usr/bin/env bash
# Runs the Java reference server (pinned ref) on the host JVM, with PostGIS in Docker.
# Used for measuring Java behaviour, diffcheck and shadow runs during development.
#
#   dev/java-local.sh start   # DB container + Java on http://localhost:8081 (admin/district)
#   dev/java-local.sh stop
#   dev/java-local.sh status
#   dev/java-local.sh logs
#   dev/java-local.sh psql [args]
#
# Environment:
#   JAVA_APP    the Jib app layout to run (default: target/java/app), copied from the image
#               built at ci/dhis2-core.ref:  cid=$(docker create dhis2/core-dev:local);
#               docker cp $cid:/app target/java/app; docker rm $cid
#               It is run like the image does: java -cp @jib-classpath-file <jib-main-class>.
#   DHIS2_JAVA_HOME  a JDK 17 (the image itself runs Java 21; use 21 when measuring JVM-sensitive output) (default: Homebrew openjdk@17; the shell's JAVA_HOME is ignored
#               because it is often an older JDK)
#   DB_DUMP     optional .sql.gz to load into a fresh DB (e.g. the Sierra Leone demo dump)
#   JAVA_PORT   default 8081; DB_PORT default 55432
set -euo pipefail

repo=$(cd "$(dirname "$0")/.." && pwd)
run="$repo/target/dev"
home="$run/dhis2_home"
app=${JAVA_APP:-$repo/target/java/app}
java_port=${JAVA_PORT:-8081}
db_port=${DB_PORT:-55432}
db_container=dhis2rs-dev-db
db_image=postgis/postgis:16-3.5-alpine
JAVA_HOME=${DHIS2_JAVA_HOME:-/opt/homebrew/opt/openjdk@17/libexec/openjdk.jdk/Contents/Home}

start_db() {
  if docker ps --format '{{.Names}}' | grep -qx "$db_container"; then return; fi
  if docker ps -a --format '{{.Names}}' | grep -qx "$db_container"; then
    docker start "$db_container" > /dev/null
  else
    docker run -d --name "$db_container" -p "127.0.0.1:$db_port:5432" \
      -e POSTGRES_USER=dhis -e POSTGRES_PASSWORD=dhis -e POSTGRES_DB=dhis -e LC_COLLATE=C \
      "$db_image" postgres -c max_locks_per_transaction=100 > /dev/null
  fi
  until docker exec "$db_container" pg_isready -U dhis -d dhis > /dev/null 2>&1; do sleep 1; done
  if [[ -n "${DB_DUMP:-}" ]]; then
    echo "loading $DB_DUMP ..."
    gunzip -c "$DB_DUMP" | docker exec -i "$db_container" psql -q -U dhis -d dhis > "$run/db-load.log" 2>&1
  fi
}

write_conf() {
  mkdir -p "$home"
  cat > "$home/dhis.conf" <<CONF
connection.dialect = org.hibernate.dialect.PostgreSQLDialect
connection.driver_class = org.postgresql.Driver
connection.url = jdbc:postgresql://localhost:$db_port/dhis
connection.username = dhis
connection.password = dhis
server.base.url = http://localhost:$java_port/
CONF
}

case "${1:-}" in
  start)
    [[ -f "$app/jib-classpath-file" ]] || { echo "no Jib layout in $app (see JAVA_APP)" >&2; exit 2; }
    mkdir -p "$run"
    start_db
    write_conf
    if [[ -f "$run/java.pid" ]] && kill -0 "$(cat "$run/java.pid")" 2> /dev/null; then
      echo "already running (pid $(cat "$run/java.pid"))"
      exit 0
    fi
    classpath=$(sed "s#/app/#$app/#g; s#/app:#$app:#g" "$app/jib-classpath-file")
    DHIS2_HOME="$home" SERVER_PORT="$java_port" nohup "$JAVA_HOME/bin/java" -Xmx3g \
      -cp "$classpath" "$(cat "$app/jib-main-class-file")" > "$run/java.log" 2>&1 &
    echo $! > "$run/java.pid"
    echo -n "waiting for Java on :$java_port "
    until curl -sf -u admin:district -o /dev/null "http://localhost:$java_port/api/system/ping"; do
      kill -0 "$(cat "$run/java.pid")" 2> /dev/null || { echo; tail -30 "$run/java.log"; exit 1; }
      echo -n .; sleep 5
    done
    echo " up"
    ;;
  stop)
    [[ -f "$run/java.pid" ]] && kill "$(cat "$run/java.pid")" 2> /dev/null || true
    rm -f "$run/java.pid"
    docker stop "$db_container" > /dev/null 2>&1 || true
    ;;
  status)
    if [[ -f "$run/java.pid" ]] && kill -0 "$(cat "$run/java.pid")" 2> /dev/null; then
      echo "java: running (pid $(cat "$run/java.pid")) on :$java_port"
    else
      echo "java: stopped"
    fi
    docker ps --filter "name=$db_container" --format 'db: {{.Status}} {{.Ports}}'
    ;;
  logs) tail -f "$run/java.log" ;;
  psql) shift; docker exec -i "$db_container" psql -U dhis -d dhis "$@" ;;
  *) sed -n '2,20p' "$0"; exit 2 ;;
esac
