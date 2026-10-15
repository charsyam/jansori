1. Jansori 는 Mysql 의 성능지표와 explain을 통해서 해당 쿼리가 실제로 index를 잘 타고 있는지 어떤 쿼리가 가장 많이 들어오는지를
 시각적으로 보여주는 툴입니다.
2. rust로 구현해서 아주 간단하게 사용하고, 리소스를 적게 먹어야 합니다.
3. mysql 의 performance_schema.events_statements_history_long 도 사용하고, 사용할 수 있는 정보는 모두 알아야 함.
4. web 기능을 제공해서, 화면에서 바로 어떤 호스트가 많이 접속하고, 어떤 쿼리가 많이 호출되고, 각각이 어떤 실행 plan을 가지고
 동작하는지를 보여줄 수 있어야 함.
5. web에서는 filtering 과 sort이 가능해야 함.
