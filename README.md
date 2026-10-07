# Jansori

MySQL과 PostgreSQL 쿼리 활동, 호스트, 테이블 구조, 추정 실행 계획을 보는 작은 Rust 웹 도구입니다.

## 실행

Rust 1.94 이상이 필요합니다.

```bash
DATABASE_URL='mysql://jansori:password@127.0.0.1:3306/app' cargo run --release
```

PostgreSQL은 `postgres://` 또는 `postgresql://` URL을 사용합니다. 웹 화면은 기본적으로 `http://127.0.0.1:8080`에서 열립니다. `JANSORI_BIND`와 `JANSORI_POLL_MS`로 주소와 MySQL 로그 조회 주기를 바꿀 수 있습니다. 서버는 원본 SQL을 보여주므로 외부에 공개할 때는 별도 인증 계층이 필요합니다.

Jansori는 DB 풀의 모든 새 연결에 읽기 전용 트랜잭션 기본값을 설정하고 시작 시 적용 여부를 확인합니다. DB 권한도 읽기 전용으로 제한된 전용 계정을 사용하세요. 세션 설정은 높은 권한을 가진 계정의 권한 자체를 제거하지는 않습니다.

웹의 수집 상태는 사용 가능 여부를 색과 스위치 모양으로 표시합니다. 이 스위치는 상태 표시이며 DB 설정을 변경하지 않습니다. 읽기 전용 연결 상태도 함께 확인할 수 있습니다. 웹 자체에는 로그인 기능이 없으므로 네트워크 접근은 별도로 제한해야 합니다.

## MySQL 수집 설정

이 버전의 쿼리 호출 수는 `mysql.general_log` 테이블에서 수집합니다. `performance_schema`가 꺼져 있어도 동작합니다. 관리자 계정에서 다음을 설정합니다.

```sql
SET GLOBAL log_output = 'TABLE';
SET GLOBAL general_log = 'ON';
CREATE USER 'jansori'@'127.0.0.1' IDENTIFIED BY 'change-me';
GRANT SELECT ON mysql.general_log TO 'jansori'@'127.0.0.1';
GRANT SELECT ON app.* TO 'jansori'@'127.0.0.1';
```

모니터링에는 전용 계정을 사용합니다. Jansori는 해당 계정의 쿼리를 집계에서 제외합니다. 로그를 켠 뒤 Jansori가 시작한 시점부터의 쿼리만 집계하며, 재시작하면 메모리 집계가 초기화됩니다. `general_log`에는 실행 시간이 없어서 화면에는 `정보 없음`으로 표시합니다. 같은 SQL을 정확한 문자열 기준으로 묶으며, 리터럴 값이 다른 SQL은 별도로 표시합니다.

`mysql.general_log`의 기본 저장 엔진은 CSV이고 인덱스가 없어 로그가 커질수록 주기적 조회 비용이 증가할 수 있습니다. 현재 구현은 한 번에 2,000행을 넘으면 일부를 누락하는 대신 오류를 냅니다. SQL은 앞 8,192자만 메모리에 보관하고 전체 SQL의 해시로 구분합니다. 로그 회전과 보존 정책을 운영 환경에 맞게 설정해야 합니다. 쿼리 텍스트와 접속 정보가 로그에 남으므로 접근 권한을 제한하세요.

계정 변경, 권한 변경, `SET` 명령은 화면에서 SQL을 가립니다. 일반 쿼리의 리터럴 값은 표시될 수 있습니다.

## PostgreSQL 수집 설정

쿼리별 호출 수와 실행 시간에는 `pg_stat_statements`가 필요합니다. 설치되지 않아도 현재 접속 호스트는 확인할 수 있습니다. 쿼리 행을 클릭해 실행 계획을 조회하려면 수집된 쿼리가 필요합니다. `pg_stat_statements` 활성화에는 서버 설정과 재시작이 필요합니다.

```sql
-- postgresql.conf: shared_preload_libraries = 'pg_stat_statements'
CREATE EXTENSION IF NOT EXISTS pg_stat_statements;
CREATE ROLE jansori LOGIN PASSWORD 'change-me';
GRANT pg_read_all_stats TO jansori;
GRANT CONNECT ON DATABASE app TO jansori;
GRANT USAGE ON SCHEMA public TO jansori;
GRANT SELECT ON ALL TABLES IN SCHEMA public TO jansori;
```

PostgreSQL도 전용 모니터링 계정을 사용합니다. 이 계정의 쿼리는 통계 화면에서 제외됩니다. `pg_stat_statements`의 SQL은 정규화되어 값이 `$1` 같은 자리표시자로 보일 수 있습니다.
PostgreSQL 16 이상에서는 쿼리 행을 클릭하면 자리표시자를 유지한 범용 추정 계획을 자동으로 보여줍니다. 이 계획은 특정 값으로 실행했을 때의 계획과 다를 수 있습니다.

## 기간별 호출 수

웹 화면에는 전체 누적·기간 선택 버튼, 기간별 호출 그래프, 쿼리·호스트 탭 순서로 표시됩니다. 쿼리 탭의 기본 목록은 전체 누적 통계이며, 첫 화면의 그래프는 Jansori 실행 이후 최근 7일 호출 추이입니다. 페이지당 10·25·50·100개를 선택하고 이전·다음 페이지로 이동할 수 있으며 전체 쿼리 수가 표시됩니다. 1분, 5분, 10분, 1시간, 3시간, 1일, 3일, 7일 중 조회 기간을 선택하면 해당 기간의 쿼리 목록과 호출 그래프가 함께 바뀝니다. 최근 1분은 5초 단위 막대 12개로 표시하며, PostgreSQL은 수집 시각, MySQL은 general log 기록 시각을 기준으로 합니다. 시작·끝 시각을 직접 지정하거나 1분보다 긴 기간의 그래프 막대를 클릭해 특정 구간으로 좁힐 수도 있습니다. 끝 분은 포함합니다. 쿼리 행을 선택하면 해당 쿼리의 추이로 바뀝니다.

MySQL은 general query log에 기록된 **서버 수신 시각**을 사용합니다. 이는 실제 실행 시작 시각과 다를 수 있습니다. PostgreSQL의 `pg_stat_statements`는 누적 지표만 제공하므로, Jansori가 직전 조회 이후 늘어난 호출 수와 실행 시간을 **수집 시각**의 분에 배정합니다. 첫 조회는 기준값만 저장하므로 이전 실행분은 기간 통계에 나타나지 않습니다. 두 방식 모두 분 단위 통계를 프로세스 메모리에 최대 7일 보관하며, 재시작하면 초기화됩니다. 그래프는 조회 기간에 맞춰 분 단위 데이터를 묶어 표시합니다.

총 실행 시간과 평균 실행 시간의 단위는 **ms**이며 화면에서 소수점 다섯 자리까지 표시합니다. PostgreSQL 기간 통계의 총·평균 시간은 수집한 실행 시간 증가분으로 계산합니다. MySQL general query log에는 실행 시간이 없어 `정보 없음`으로 표시합니다.

## API

| 경로 | 내용 |
| --- | --- |
| `GET /api/info` | DB 종류와 버전 |
| `GET /api/capabilities` | 수집 기능별 사용 가능 상태와 이유 |
| `GET /api/queries?window=1h&sort=calls&search=...&host=...` | 기간별 쿼리 통계. `window`는 `1m`, `5m`, `10m`, `1h`, `3h`, `1d`, `3d`, `7d` |
| `GET /api/queries/page?limit=10&offset=0&window=1h` | 페이지별 쿼리 목록, 전체 건수와 건수 정확성. `window`를 빼면 누적 통계 |
| `GET /api/hosts` | 호스트별 정보 |
| `GET /api/recent` | 최근 문장. PostgreSQL 기본 통계에서는 사용 불가 |
| `GET /api/timeline?window=1h&query_key=...` | 기간별 호출 수와 시각 기준. `query_key`는 선택 사항 |
| `GET /api/queries/{key}/explain` | 수집된 쿼리 키의 추정 실행 계획. 임의 SQL 입력 API는 제공하지 않음 |

쿼리 행을 클릭하면 현재 최적화 계획을 보여줍니다. `EXPLAIN ANALYZE`를 실행하지 않으며 과거 실행의 실제 계획은 아닙니다. 수동 SQL 입력 화면과 API는 없습니다. MySQL general log에는 기본 스키마가 없어서 쿼리 통계의 스키마 필터를 제공하지 않습니다. PostgreSQL 쿼리 통계에는 호스트별 집계와 검사 행 수가 없습니다.

기간 프리셋 대신 `from`과 `to` Unix 초를 두 API에 함께 전달하면 최근 7일 안의 직접 지정 범위를 조회합니다. 둘은 `window`와 동시에 사용할 수 없습니다. `/api/timeline`은 기존 `hours` 매개변수도 지원합니다. 응답의 `bucket_seconds`는 그래프 막대 하나의 길이입니다.
