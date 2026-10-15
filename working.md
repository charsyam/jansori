# Jansori 작업 현황

마지막 갱신: 2026-10-15

상태 표기: `완료` / `진행 중` / `대기` / `결정 필요`

## 현재 상태

| 작업 | 상태 | 메모 |
| --- | --- | --- |
| `prd.md` 검토 | 완료 | 요구사항 5개 확인 |
| MySQL 수집원과 EXPLAIN 제약 조사 | 완료 | MySQL 8.4 공식 문서 기준으로 초안 작성 |
| 구현 방향 제안 | 완료 | MySQL과 PostgreSQL을 공통 백엔드 인터페이스로 연결하는 방향으로 수정 |
| 지원 DB 버전 및 데이터 보존 방식 결정 | 결정 필요 | 버전별 컬럼/기능 탐지, 메모리 내 짧은 기간 추이를 초기안으로 둠 |
| Rust 프로젝트 생성 및 기본 설정 | 완료 | 라이브러리 크레이트 생성, 빌드 산출물 제외 설정 |
| 공통 백엔드 인터페이스 | 완료 | DB 정보·기능 상태·실행 계획·쿼리/호스트 통계 계약 작성. general log의 실행 시간 부재도 모델에 반영 |
| MySQL 어댑터 | 완료 | SELECT EXPLAIN, general query log 통계 구현 |
| MySQL 전체 쿼리 수집 경로 | 완료 | Performance Schema가 꺼진 경우 general query log 사용. 프록시 사용하지 않음 |
| General query log 수집기 | 완료 | TABLE 출력에서 새 행 수집, 동일 타임스탬프 중복 제거, 리터럴 값 정규화 집계, 표시 길이·배치·메모리 상한 |
| PostgreSQL 어댑터 | 완료 | SELECT EXPLAIN, pg_stat_statements 쿼리 통계, 현재 호스트 연결 수 |
| 수집 기능 진단 | 완료 | MySQL general_log/TABLE, PostgreSQL pg_stat_statements 활성화 상태 표시 |
| 쿼리/호스트 통계 API | 완료 | 검색·정렬·페이지네이션 API. 수집원에 없는 지표의 정렬은 사용 불가 응답 |
| 안전한 실행 계획 조회 | 완료 | 단일 SELECT의 일반 EXPLAIN만 제공. 실제 실행 없음 |
| 웹 대시보드 | 완료 | 기간별 호출, 쿼리, 호스트, 실행 계획 화면 |
| 실제 DB 통합 검증 및 문서화 | 완료 | MySQL 8.4.4 및 PostgreSQL 16.15 임시 서버로 핵심 API 확인; README 작성 |
| 재시작 후 장기 보존 | 대기 | 분 단위 기간 통계는 최대 7일간 프로세스 메모리에 보관. 재시작하면 초기화 |
| General log 수집 부하 개선 | 대기 | 기본 CSV 로그 테이블 전체 조회 비용 해결 필요. 파일 tail 방식 검토 |
| PostgreSQL 웹 데모 실행 | 대기 | 이전에 `0.0.0.0:38080`에서 실행했으나 현재 실행 세션이 끊겼고, 새 실행 환경의 네트워크 제한으로 실제 상태 확인·재실행 불가 |
| 쿼리 클릭 시 실행 계획 자동 표시 | 완료 | 선택한 쿼리 바로 아래에 추정 계획과 원본 JSON 표시. PostgreSQL `$1` 쿼리는 범용 계획 사용 |
| 기간별 호출 통계 | 완료 | UTC 기준 분 단위로 최대 7일 메모리 보관. MySQL은 general log 수신 시각, PostgreSQL은 누적 지표의 수집 시각 기준 |
| 시간 범위 선택 | 완료 | 시작·끝 분 또는 그래프 막대 클릭으로 쿼리 목록과 그래프를 필터링. 종료 분 포함 |
| 실행 시간 표시 단위 | 완료 | 총·평균 시간은 ms, 화면에서는 소수점 다섯 자리. PostgreSQL 기간별 실행 시간 증가분 저장, MySQL은 정보 없음 |
| 쿼리·호스트 탭 | 완료 | 기간별 호출 그래프는 탭 위, 쿼리 탭에 쿼리·실행 계획, 호스트 탭에 호스트 현황 배치. 키보드 좌우 화살표 전환 지원 |
| 기간 프리셋 1분~7일 | 완료 | 1분·5분·10분은 5초 기록, 1시간~7일은 분 단위 기록 및 기간별 그래프 묶음. 쿼리 목록과 그래프에 함께 적용 |
| 쿼리 목록 페이지 | 완료 | 전체 누적 조회를 기본으로 추가하고 페이지당 10·25·50·100개, 전체 건수, 이전·다음 페이지 구현. 기존 `/api/queries` 유지 |
| 첫 화면 그래프 | 완료 | 전체 누적 목록 아래에 Jansori 실행 이후 최근 7일 호출 그래프를 표시하고 목록과 그래프의 기간 차이를 명시 |
| 읽기 전용 연결·수동 EXPLAIN 제거 | 완료 | DB 풀의 새 연결을 읽기 전용으로 설정하고 시작 시 상태 확인. 실행 계획 API는 수집된 쿼리 키만 허용하고 수동 SQL 입력 화면 제거 |
| 수집 상태 시각화 | 완료 | 기능별 사용 가능 여부를 정적 스위치 모양의 상태 카드로 표시. 읽기 전용 연결 상태 포함 |
| 장애 시 DB 첫 화면 적합성 검토 | 완료 | 현재 쿼리 증가·계획 확인에는 유용하나, 활성 세션·잠금 대기·연결 포화·수집 신선도가 없어 장애 원인 1차 분류 화면으로는 부족 |
| 장애 첫 화면 핵심 지표 | 대기 | 연결 수/한도, 실행·대기 세션, 오래 실행 중인 쿼리·트랜잭션, 차단 세션, 수집 마지막 성공/오류/지연. MySQL 수집 비용과 누락도 노출 필요 |
| 최근 1분 조회 | 완료 | 5초 단위 최근 1분 쿼리 목록·호출 그래프를 PostgreSQL·MySQL에 적용. 기존 장기 분 단위 통계와 병행 |
| 테이블 설명 제거 | 완료 | 화면, API, 공통 인터페이스 및 DB별 조회 코드 제거 |
| 기간별 호출 그래프 이동 | 완료 | 쿼리·호스트 탭 위로 이동해 첫 화면에서 먼저 보이도록 배치 |
| 기간 선택 버튼 이동 | 완료 | 전체 누적·1분~7일 선택을 기간별 호출 그래프 위에 배치 |
| 재시작 직후 빈 그래프 안내 | 완료 | 메모리 추이 초기화로 0회일 때 그래프 영역에 안내를 표시하고 데모 호출 재생성 |
| 5분 기간 경계 수정 | 완료 | 5분·10분 조회를 5초 기록으로 집계해 분 경계 직전 호출 누락 방지 |
| MySQL 값별 쿼리 중복 집계 수정 | 완료 | 같은 DB에서 숫자·작은따옴표 리터럴만 다른 SQL을 한 쿼리로 집계하고 실행 계획은 실제 SQL 예시로 조회 |
| 데이터베이스 선택 | 완료 | DB 목록을 조회하고 쿼리·호스트·추이를 선택한 DB로 필터링. MySQL은 수집 중 확인한 기본 DB 기준 |
| 목록의 예상 행·비용 | 완료 | 현재 페이지의 SELECT 추정 계획을 제한된 동시 요청으로 조회해 목록에 표시 |
| 데모 사용자 테이블 이름 정리 | 대기 | 사용자가 데이터 흐름 확인을 위해 잠시 중단 요청. DB 테이블과 통계는 아직 변경하지 않음 |
| `daily_charsyam` MySQL 접속 확인 | 대기 | 사용자 터미널에서는 0:3306 접속 시 `mysql_native_password` 미로드 오류. 배포 설정은 MySQL 8.4이며 해당 인증 플러그인은 기본 비활성화. 이 실행 환경의 소켓 연결은 EPERM이라 직접 인증은 아직 불가 |
| `daily_mysql` 서비스 상태 확인 | 대기 | Jansori 중단 제보를 MySQL 중단으로 잘못 해석했음. MySQL 중단 여부는 요청되지 않았고 상태 미확인 |
| Jansori 웹 상태 복구 | 대기 | 사용자가 Jansori 중단 제보. 이전 PTY 세션 ID는 더 이상 유효하지 않고 현재 실행 환경에서는 로컬 TCP 소켓 연결 불가. 호스트 터미널에서 프로세스·포트 확인 후 재실행 필요 |

## 구현 설계 초안

1. **단일 Rust 실행 파일**: 선택한 데이터베이스를 주기적으로 읽는 수집기와 HTTP 서버를 한 프로세스에서 실행한다. MySQL과 PostgreSQL 어댑터가 같은 `DatabaseBackend` 계약을 구현한다. 설정은 환경 변수 또는 설정 파일로 받고, 정적 웹 자산을 실행 파일에 포함한다.
2. **수집원 분리**: 현재 MySQL은 general query log TABLE 출력의 수신 쿼리를 메모리에서 집계한다. PostgreSQL은 `pg_stat_statements`를 읽는다. MySQL Performance Schema 집계는 향후 추가 가능하지만 현재 구현에는 없다. 수집원별 제공 지표 차이를 API와 UI에서 구분한다.
3. **부하 관리**: 한 번에 가져오는 general log 행과 메모리에 보관하는 쿼리·최근 문장 수에 상한을 둔다. 다만 기본 CSV 로그 테이블에는 인덱스가 없어 로그가 커지면 조회 비용이 증가한다. 분 단위 통계는 최대 7일만 보존하며, 재시작 후 보존과 파일 기반 tail 수집은 후속 과제다.
4. **실행 계획**: 요청 시 단일 SELECT의 추정 계획을 조회한다. MySQL은 `EXPLAIN FORMAT=JSON`, PostgreSQL은 `EXPLAIN (FORMAT JSON)`을 쓴다. 과거 실행의 실제 계획과 구분해 표시한다.
5. **웹 UI**: 기간별 호출, 쿼리·호스트 순위, 실행 계획, 수집 상태를 제공한다. 텍스트·호스트 필터, 기간 프리셋과 직접 지정 범위, 수집 가능한 지표의 정렬을 제공한다.
6. **PostgreSQL**: 계획은 `EXPLAIN (FORMAT JSON)`으로 얻는다. 쿼리별 집계는 `pg_stat_statements`를 사용하되, 확장이 없으면 기능을 사용 불가로 표시한다. 최근 완료 문장 목록은 기본 통계만으로 제공할 수 없으므로 공통 기능 상태에 반영한다.
7. **MySQL 수집 상태별 동작**: Performance Schema 설정과 관계없이 general query log TABLE 출력을 사용한다. 꺼져 있으면 원인을 화면에 표시하며 서버 설정을 자동 변경하지 않는다. 로그에는 실행 시간이 없으므로 시간 필드는 `None`이다. 프록시는 사용하지 않는다.

## 확인할 결정

- 최소 지원 MySQL 버전: 우선 8.0과 8.4를 제안한다. 버전별 컬럼 차이는 시작 시 탐지한다.
- 추이 보존: 우선 메모리 내 짧은 기간만 보존하고, 재시작을 넘어선 장기 보존이 필요하면 별도 저장소를 추가한다.
- 웹 접근 범위: 우선 로컬 바인딩을 기본으로 하고, 외부 공개가 필요할 때 인증과 TLS 구성을 정한다.

## 검증 기록

- 2026-10-15: `cargo test --offline` 10개 통과, 웹 스크립트 문법 검사와 `git diff --check` 통과. 새 빌드로 `jansori-daily`를 재시작하고 기본 및 DB 필터 쿼리·호스트·추이 API 200 확인.
- 2026-10-07: 임시 Rust 1.99.0 도구체인으로 `cargo check` 통과. `cargo fmt` 적용. 실제 DB 어댑터는 아직 구현 전이다.
- 2026-10-07: general query log 수집을 결정하고 공통 통계 모델에 수집원 및 선택적 실행 시간 필드 추가.
- 2026-10-07: `cargo test` 4개 통과. MySQL 8.4.4에서 `performance_schema=OFF`/general log TABLE로 테이블 설명, EXPLAIN, 쿼리 호출 수, 호스트 집계를 확인. PostgreSQL 16.15에서 pg_stat_statements 활성화 전후, 테이블 설명, EXPLAIN, 쿼리 통계를 확인. 실제 DB 검증 중 MySQL `USE` 프로토콜과 인덱스 컬럼 타입 오류를 수정.
- 2026-10-07: 모니터링 계정의 자체 조회를 집계에서 제외하고 계정·권한 변경 SQL의 화면 표시를 가림. MySQL 로그 SQL 표시를 8,192자로 제한하고 전체 문장의 SHA-256 값으로 집계. 테스트용 DB와 웹 프로세스는 검증 후 종료.
- 2026-10-07: 사용자의 웹 확인 요청에 따라 새 PostgreSQL 16 데모 컨테이너를 실행하고 `pg_stat_statements`에 샘플 쿼리 40/25/12회를 생성. `http://192.168.0.112:38080/`에서 Jansori 웹 실행. `/`, `/api/capabilities`, `/api/queries` HTTP 200 및 쿼리 통계 확인. 이 데모는 이전에 종료한 검증용 서버나 사용자의 별도 DB가 아니다.
- 2026-10-07: 쿼리 클릭 시 표 바로 아래에 EXPLAIN을 자동 펼치도록 수정. PostgreSQL 16의 `GENERIC_PLAN`으로 `$1` 정규화 쿼리의 추정 계획을 생성하고, 인덱스 스캔과 순차 스캔 응답을 실제 데모 DB에서 확인. `cargo test` 4개 통과, 브라우저 스크립트 문법 검사 통과. 웹 프로세스를 새 빌드로 재시작.
- 2026-10-07: 최근 24시간 그래프 및 선택 쿼리 추이 추가. PostgreSQL 데모에서 새 쿼리 3회 후 쿼리별 3회와 전체 증가분을 확인. MySQL 8.4.4 `performance_schema=OFF`/general log TABLE에서 수신 시각 버킷과 쿼리별 호출 수 확인. 검증용 MySQL은 종료.
- 2026-10-07: 시작·끝 시간대 필터와 그래프 막대 선택, ms 표기를 추가. `cargo test` 6개 통과, 웹 스크립트 문법 검사 통과. PostgreSQL 데모 API에서 현재 시간대 전체 3회·선택 쿼리 2회, 이전 시간대 0회, 범위 밖 요청 400 응답 확인. 웹 프로세스를 새 빌드로 `0.0.0.0:38080`에서 재실행하고 샘플 쿼리를 다시 생성.
- 2026-10-07: 쿼리·호스트 탭을 추가하고 새 빌드로 웹 재실행. 탭 마크업과 웹 스크립트 문법 확인. 데모 PostgreSQL에서 새 샘플 호출 후 시간대 API 4회와 범위 쿼리 2건 확인. 웹 URL `http://192.168.0.112:38080/`.
- 2026-10-07: 조회 기간 프리셋 5분·10분·1시간·3시간·1일·3일·7일 구현. 통계를 1분 단위로 최대 7일 저장하고 그래프는 기간에 따라 1분~3시간 묶음으로 제공. PostgreSQL 실행 시간 증가분도 저장해 기간별 총·평균 시간을 계산. `cargo test` 6개, 웹 스크립트 문법 검사 통과. PostgreSQL 16 데모의 일곱 프리셋에서 버킷 수·호출 수 확인, `pg_sleep`로 5분 총·평균 실행 시간 확인. MySQL 8.4.4 general log TABLE에서 5분·7일 프리셋 호출 집계 확인. 검증용 MySQL은 종료했고 PostgreSQL 웹은 `http://192.168.0.112:38080/`에서 실행 중.
- 2026-10-07: 쿼리 목록에 전체 누적 기본 화면과 페이지당 10·25·50·100개 선택, 전체 건수, 이전·다음 페이지 추가. PostgreSQL 데모의 누적 통계 17건을 첫 페이지 10건·다음 페이지 7건으로 분리하고 쿼리 중복이 없는지 확인. 기간별 목록은 Jansori 시작 이후의 호출만 담으므로 재시작 직후 적게 보일 수 있음을 화면에 명시. `cargo test` 6개와 웹 스크립트 문법 검사 통과. 새 빌드로 `http://192.168.0.112:38080/` 재실행 후 1시간 기간의 샘플 쿼리 3건도 확인.
- 2026-10-07: 전체 누적 기본 화면에서 숨겨져 있던 그래프를 다시 표시. 그래프는 최근 7일 관측분이며 누적 쿼리 목록과 기간이 다름을 화면에 명시. 웹 스크립트 문법, 초기 그래프 마크업, 실제 `/api/timeline?hours=168`의 56개 버킷과 샘플 호출 4회 확인. 웹을 새 빌드로 재실행.
- 2026-10-07: MySQL과 PostgreSQL 풀의 모든 새 연결에 읽기 전용 트랜잭션 기본값을 설정하고 시작 시 적용 상태를 확인. 수동 SQL EXPLAIN UI와 `POST /api/explain` 제거; 수집된 쿼리 키의 `GET /api/queries/{key}/explain`만 제공. PostgreSQL 16 데모에서 키 기반 Index Scan과 이전 API 404 확인, 모니터 계정의 SELECT는 허용되고 INSERT·UPDATE·DELETE는 불가함을 확인. MySQL 8.4.4 임시 서버에서 읽기 전용 연결로 쿼리 조회 및 키 기반 EXPLAIN 확인 후 임시 서버 종료. `cargo test` 6개, 웹 스크립트 문법 검사 통과. 새 빌드로 웹 재실행.
- 2026-10-07: 수집 상태를 사용 가능·불가 카드와 정적 스위치 모양으로 변경하고 읽기 전용 연결 상태 추가. PostgreSQL 데모 및 MySQL 8.4.4 임시 서버에서 `/api/capabilities`의 읽기 전용 상태 `available=true`, 기능 6개 확인. MySQL 임시 서버 종료, PostgreSQL 웹은 새 빌드로 `http://192.168.0.112:38080/` 재실행. `cargo test` 6개와 웹 스크립트 문법 검사 통과. 웹에는 인증이 없어 네트워크 접근 통제는 별도 필요.
- 2026-10-07: 장애 대응용 DB 첫 화면 기준으로 기능 검토. 쿼리 변화·누적 실행 시간·추정 계획은 확인 가능하지만, 현재 연결 포화와 대기/잠금 원인, 수집 신선도는 확인 불가. MySQL general log는 실행 시간을 제공하지 않고, 한 번에 2,000행을 넘으면 수집 오류가 로그에만 기록됨. README의 제거된 수동 EXPLAIN 안내도 수정.
- 2026-10-07: 최근 1분 선택을 추가하고 5초 버킷 12개로 쿼리 목록과 그래프를 집계. PostgreSQL은 5초 수집 시각, MySQL은 general log 기록 시각을 사용. `cargo test` 7개와 웹 스크립트 문법 검사 통과. PostgreSQL 16 데모에서 1분 그래프 12개 버킷·3회 호출과 목록 2건, MySQL 8.4.4 데모에서 12개 버킷·호출 집계와 목록 4건 확인. PostgreSQL 웹을 새 빌드로 재실행.
- 2026-10-07: 테이블 설명 화면·API·백엔드 계약·DB별 조회 및 상태 필드를 제거. 기간별 호출 그래프를 쿼리·호스트 탭 위로 이동. `cargo test` 7개, 웹 스크립트 문법 검사 통과. 실행 중인 웹에서 그래프 순서, 테이블 설명 미노출, 기존 API 404, 쿼리·그래프 API 200 확인 후 새 빌드로 재실행.
- 2026-10-07: 전체 누적·1분~7일 기간 선택 버튼을 기간별 호출 그래프 위로 이동. 그래프의 누적 목록 안내 문구도 새 배치에 맞게 수정. 웹 스크립트 문법, 화면 요소 순서, 1분 그래프 API의 12개 버킷 확인 후 새 빌드로 웹 재실행.
- 2026-10-07: 그래프가 보이지 않는다는 제보를 확인. 웹 재시작으로 메모리 추이가 초기화되어 그래프 API가 0회를 반환한 것이 원인. 0회인 기간에는 그래프 영역에 안내 문구를 표시하도록 수정하고, PostgreSQL 데모 호출을 다시 생성해 7일·1분 그래프 모두 4회 집계 확인. 웹 스크립트 문법 검사 및 새 빌드 재실행 완료.
- 2026-10-07: 지속 수집 여부 점검. 실행 중인 PostgreSQL 웹에서 1시간 호출 수 4회·최근 1분 0회를 확인한 뒤 데모 쿼리를 실행하자 다음 5초 수집 주기 후 각각 7회·3회로 증가. MySQL 수집은 최소 100ms(기본 1초), PostgreSQL 수집은 최소 5초 간격이며 웹은 5초마다 갱신함을 코드에서 확인.
- 2026-10-07: 최근 5분 조회에서 실제 5분이 지나기 전에 호출이 빠질 수 있는 분 경계 문제 수정. 1·5·10분 기간은 5초 기록으로 집계하고 그래프는 각각 5초·30초·1분 단위로 표시. `cargo test` 8개와 웹 스크립트 문법 검사 통과. PostgreSQL 데모에서 새 쿼리 실행 후 1·5·10분 및 1시간 목록·그래프 모두 3회 일치 확인. 새 빌드로 웹 재실행.

## 조사 근거

- [MySQL statement event tables](https://dev.mysql.com/doc/refman/8.4/en/performance-schema-statement-tables.html)
- [MySQL statement summary tables](https://dev.mysql.com/doc/refman/8.4/en/performance-schema-statement-summary-tables.html)
- [MySQL statement digests and sampling](https://dev.mysql.com/doc/refman/8.4/en/performance-schema-statement-digests.html)
- [MySQL full table scan summary](https://dev.mysql.com/doc/refman/8.4/en/sys-statements-with-full-table-scans.html)
- [MySQL EXPLAIN](https://dev.mysql.com/doc/refman/8.4/en/explain.html)
- [MySQL Performance Schema startup](https://dev.mysql.com/doc/refman/8.4/en/performance-schema-startup-configuration.html)
- [MySQL SHOW STATUS](https://dev.mysql.com/doc/refman/8.4/en/show-status.html)
- [MySQL slow query log](https://dev.mysql.com/doc/refman/8.4/en/slow-query-log.html)
- [MySQL general query log](https://dev.mysql.com/doc/refman/8.4/en/query-log.html)
- [PostgreSQL EXPLAIN](https://www.postgresql.org/docs/current/sql-explain.html)
- [PostgreSQL pg_stat_statements](https://www.postgresql.org/docs/current/pgstatstatements.html)
